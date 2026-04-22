use anyhow::Result;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

use crate::agent::engine::AgentEngine;
use crate::config::AgentConfig;

/// Response text that heartbeat agents return when nothing needs attention.
/// Must match the instruction in AGENTS.md / HEARTBEAT.md.
const HEARTBEAT_OK: &str = "HEARTBEAT_OK";

/// A scheduled job record from the database.
#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)] // Fields used in display formatting via engine.rs handle_cron
pub struct ScheduledJob {
    pub id: Uuid,
    pub agent_id: String,
    pub name: String,
    pub cron_expr: String,
    pub timezone: String,
    pub task_message: String,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    #[sqlx(default)]
    pub silent: bool,
    #[sqlx(default)]
    pub announce_to: Option<serde_json::Value>,
    #[sqlx(default)]
    pub jitter_secs: i32,
    #[sqlx(default)]
    pub run_once: bool,
    #[sqlx(default)]
    pub run_at: Option<chrono::DateTime<chrono::Utc>>,
    #[sqlx(default)]
    pub tool_policy: Option<serde_json::Value>,
}

/// Per-agent execution lock — prevents concurrent heartbeat + cron for the same agent.
/// Shared across all scheduled jobs via Arc.
type AgentLocks = Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>;

/// Manages cron-based tasks (heartbeat, memory decay, dynamic user-created jobs).
pub struct Scheduler {
    scheduler: JobScheduler,
    /// Maps job DB id → scheduler job UUID for removal.
    dynamic_jobs: RwLock<HashMap<Uuid, Uuid>>,
    /// Maps agent name → scheduler job UUIDs (heartbeat) for hot removal.
    agent_jobs: RwLock<HashMap<String, Vec<Uuid>>>,
    /// Broadcast channel to notify UI about session updates.
    ui_event_tx: tokio::sync::broadcast::Sender<String>,
    /// Per-agent execution lock — if agent is already running a scheduled task, skip.
    agent_locks: AgentLocks,
}

impl Scheduler {
    pub async fn new(ui_event_tx: tokio::sync::broadcast::Sender<String>) -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        Ok(Self {
            scheduler,
            dynamic_jobs: RwLock::new(HashMap::new()),
            agent_jobs: RwLock::new(HashMap::new()),
            ui_event_tx,
            agent_locks: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
        })
    }

    /// Construct a no-op `Arc<Scheduler>` for unit tests.
    /// The underlying `JobScheduler` is created but never started — no jobs will fire.
    #[cfg(test)]
    pub async fn new_noop() -> Arc<Self> {
        let (tx, _rx) = tokio::sync::broadcast::channel(1);
        let scheduler = JobScheduler::new().await.expect("noop scheduler");
        Arc::new(Self {
            scheduler,
            dynamic_jobs: RwLock::new(HashMap::new()),
            agent_jobs: RwLock::new(HashMap::new()),
            ui_event_tx: tx,
            agent_locks: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
        })
    }

    /// Add heartbeat job for an agent. Returns the scheduler job UUID.
    pub async fn add_heartbeat(
        &self,
        agent_cfg: &AgentConfig,
        engine: Arc<AgentEngine>,
    ) -> Result<Option<Uuid>> {
        let heartbeat = match &agent_cfg.agent.heartbeat {
            Some(hb) => hb,
            None => return Ok(None),
        };

        // tokio-cron-scheduler expects 6-field cron (sec min hour dom mon dow).
        // Normalize standard 5-field cron by prepending "0 " for seconds.
        let cron_6field = {
            let raw = heartbeat.cron.trim();
            let fields: Vec<&str> = raw.split_whitespace().collect();
            if fields.len() == 5 {
                format!("0 {raw}")
            } else {
                raw.to_string()
            }
        };

        // Convert cron hours from local timezone to UTC
        let cron_expr = if let Some(ref tz) = heartbeat.timezone {
            convert_cron_to_utc(&cron_6field, tz)
        } else {
            cron_6field
        };

        let agent_name = agent_cfg.agent.name.clone();
        let workspace_dir = crate::config::WORKSPACE_DIR.to_string();
        let announce_to = heartbeat.announce_to.clone();
        let owner_id = agent_cfg.agent.access.as_ref()
            .and_then(|a| a.owner_id.clone());
        let tz_display = heartbeat
            .timezone
            .clone()
            .unwrap_or_else(|| "UTC".to_string());

        tracing::info!(
            agent = %agent_name,
            cron = %cron_expr,
            timezone = %tz_display,
            "scheduling heartbeat"
        );

        let ui_tx = self.ui_event_tx.clone();
        let locks = self.agent_locks.clone();
        let job = Job::new_async(cron_expr.as_str(), move |_uuid, _lock| {
            let engine = engine.clone();
            let agent_name = agent_name.clone();
            let workspace_dir = workspace_dir.clone();
            let announce_to = announce_to.clone();
            let owner_id = owner_id.clone();
            let ui_tx = ui_tx.clone();
            let locks = locks.clone();
            Box::pin(async move {
                // Per-agent lock: skip if already running a scheduled task
                {
                    let mut active = locks.lock().await;
                    if active.contains(&agent_name) {
                        tracing::debug!(agent = %agent_name, "heartbeat skipped — agent busy with another scheduled task");
                        return;
                    }
                    active.insert(agent_name.clone());
                }
                tracing::info!(agent = %agent_name, "heartbeat triggered");
                let result = run_heartbeat(
                    &engine, &workspace_dir, &agent_name,
                    announce_to.as_deref(), owner_id.as_deref(),
                ).await;
                locks.lock().await.remove(&agent_name);
                match result {
                    Ok(()) => {
                        tracing::info!(agent = %agent_name, "heartbeat completed");
                        broadcast_session_event(&ui_tx, &agent_name, "heartbeat");
                    }
                    Err(e) => {
                        tracing::error!(agent = %agent_name, error = %e, "heartbeat failed");
                    }
                }
            })
        })?;

        let uuid = self.scheduler.add(job).await?;
        self.agent_jobs.write().await
            .entry(agent_cfg.agent.name.clone())
            .or_default()
            .push(uuid);
        Ok(Some(uuid))
    }

    /// Add memory temporal decay job (daily at 3:00 UTC).
    pub async fn add_memory_decay(&self, db: PgPool) -> Result<()> {
        tracing::info!("scheduling memory temporal decay (daily 03:00 UTC)");

        let job = Job::new_async("0 0 3 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                tracing::info!("memory decay triggered");
                match run_memory_decay(&db).await {
                    Ok((decayed, deleted)) => {
                        tracing::info!(decayed, deleted, "memory decay completed");
                    }
                    Err(e) => tracing::error!(error = %e, "memory decay failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add task cleanup job (daily at 4:00 UTC — delete old completed/failed tasks).
    pub async fn add_task_cleanup(&self, db: PgPool) -> Result<()> {
        tracing::info!("scheduling task cleanup (daily 04:00 UTC)");

        let job = Job::new_async("0 0 4 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                tracing::info!("task cleanup triggered");
                match run_task_cleanup(&db).await {
                    Ok((tasks, steps)) => {
                        tracing::info!(tasks, steps, "task cleanup completed");
                    }
                    Err(e) => tracing::error!(error = %e, "task cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add session cleanup job (daily at 5:00 UTC — delete old sessions).
    pub async fn add_session_cleanup(&self, db: PgPool, ttl_days: u32) -> Result<()> {
        if ttl_days == 0 {
            tracing::info!("session cleanup disabled (ttl_days = 0)");
            return Ok(());
        }
        tracing::info!(ttl_days, "scheduling session cleanup (daily 05:00 UTC)");

        let job = Job::new_async("0 0 5 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                tracing::info!("session cleanup triggered");
                match crate::db::sessions::cleanup_old_sessions(&db, ttl_days).await {
                    Ok(deleted) => {
                        if deleted > 0 {
                            tracing::info!(deleted, "session cleanup completed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "session cleanup failed"),
                }
                // Prune old WAL events alongside session cleanup
                match crate::db::session_wal::prune_old_events(&db, ttl_days).await {
                    Ok(pruned) => {
                        if pruned > 0 {
                            tracing::info!(pruned, "WAL event pruning completed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "WAL event pruning failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Phase 62 RES-03: hourly batched cleanup of `session_events` WAL rows.
    ///
    /// Cron `0 0 * * * *` fires at the top of every hour (`sec=0 min=0 hour=* *`
    /// — 6-field tokio-cron-scheduler format). The job calls
    /// `prune_old_events_batched`, which deletes at most `batch_size` rows per
    /// iteration to avoid long table locks and WAL bloat.
    ///
    /// `retention_days = 0` disables the hourly cleanup (returns `Ok(())` without
    /// registering a job). Errors surfaced by `prune_old_events_batched` are
    /// logged at WARN and never crash the scheduler — cleanup is best-effort.
    pub async fn add_session_events_cleanup_hourly(
        &self,
        db: PgPool,
        retention_days: u32,
        batch_size: i64,
    ) -> Result<()> {
        if retention_days == 0 {
            tracing::info!("session_events hourly cleanup disabled (retention_days = 0)");
            return Ok(());
        }
        tracing::info!(
            retention_days,
            batch_size,
            "scheduling hourly session_events cleanup (RES-03)"
        );

        let job = Job::new_async("0 0 * * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                match crate::db::session_wal::prune_old_events_batched(
                    &db,
                    retention_days,
                    batch_size,
                )
                .await
                {
                    Ok(deleted) if deleted > 0 => {
                        tracing::info!(deleted, "session_events hourly cleanup completed");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "session_events hourly cleanup failed (non-fatal)"
                        );
                    }
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add `pending_messages` cleanup job (daily at 6:30 UTC — delete rows older than 7 days).
    pub async fn add_pending_messages_cleanup(&self, db: PgPool) -> Result<()> {
        tracing::info!("scheduling pending_messages cleanup (daily 06:30 UTC)");

        let job = Job::new_async("0 30 6 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                match crate::db::pending::cleanup_old(&db, 7 * 24).await {
                    Ok(deleted) => {
                        if deleted > 0 {
                            tracing::info!(deleted, "pending_messages cleanup completed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "pending_messages cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add outbound queue cleanup job (daily at 06:45 UTC).
    /// Deletes acked items older than 7 days.
    pub async fn add_outbound_queue_cleanup(&self, db: PgPool) -> Result<()> {
        tracing::info!("scheduling outbound_queue cleanup (daily 06:45 UTC)");

        let job = Job::new_async("0 45 6 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                match crate::db::outbound::cleanup_old(&db, 7).await {
                    Ok(deleted) => {
                        if deleted > 0 {
                            tracing::info!(deleted, "outbound_queue cleanup completed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "outbound_queue cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add audit event cleanup job (daily at 6:00 UTC).
    pub async fn add_audit_cleanup(&self, db: PgPool, retention_days: u32) -> Result<()> {
        if retention_days == 0 {
            tracing::info!("audit cleanup disabled (retention_days = 0)");
            return Ok(());
        }
        tracing::info!(retention_days, "scheduling audit cleanup (daily 06:00 UTC)");

        let job = Job::new_async("0 0 6 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                match crate::db::audit::cleanup_old_events(&db, retention_days).await {
                    Ok(deleted) => {
                        if deleted > 0 {
                            tracing::info!(deleted, "audit cleanup completed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "audit cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add tool audit log cleanup job (daily at 06:15 UTC — delete entries older than 90 days).
    pub async fn add_tool_audit_cleanup(&self, db: PgPool, retention_days: u32) -> Result<()> {
        if retention_days == 0 {
            tracing::info!("tool audit cleanup disabled (retention_days = 0)");
            return Ok(());
        }
        tracing::info!(retention_days, "scheduling tool audit cleanup (daily 06:15 UTC)");

        let job = Job::new_async("0 15 6 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                match crate::db::tool_audit::cleanup_old_entries(&db, retention_days).await {
                    Ok(deleted) => {
                        if deleted > 0 {
                            tracing::info!(deleted, "tool audit cleanup completed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "tool audit cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add `usage_log` cleanup job (daily at 07:00 UTC — delete entries older than 90 days).
    pub async fn add_usage_log_cleanup(&self, db: PgPool) -> Result<()> {
        tracing::info!("scheduling usage_log cleanup (daily 07:00 UTC) retention_days=90");

        let job = Job::new_async("0 0 7 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                let result = sqlx::query("DELETE FROM usage_log WHERE created_at < now() - interval '90 days'")
                    .execute(&db).await;
                match result {
                    Ok(r) => if r.rows_affected() > 0 {
                        tracing::info!(deleted = r.rows_affected(), "usage_log cleanup");
                    },
                    Err(e) => tracing::warn!(error = %e, "usage_log cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add memory chunks decay cleanup job (daily at 08:00 UTC).
    /// Removes very old, low-score, non-pinned chunks.
    pub async fn add_memory_decay_cleanup(&self, db: PgPool) -> Result<()> {
        tracing::info!("scheduling memory decay cleanup (daily 08:00 UTC)");

        let job = Job::new_async("0 0 8 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                let result = sqlx::query(
                    "DELETE FROM memory_chunks WHERE pinned = false AND relevance_score < 0.1 AND accessed_at < now() - interval '180 days'"
                ).execute(&db).await;
                match result {
                    Ok(r) => if r.rows_affected() > 0 {
                        tracing::info!(deleted = r.rows_affected(), "memory_chunks decay cleanup");
                    },
                    Err(e) => tracing::warn!(error = %e, "memory_chunks decay cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add notifications cleanup job (daily at 08:30 UTC — delete entries older than 30 days).
    pub async fn add_notifications_cleanup(&self, db: PgPool) -> Result<()> {
        tracing::info!("scheduling notifications cleanup (daily 08:30 UTC)");

        let job = Job::new_async("0 30 8 * * *", move |_uuid, _lock| {
            let db = db.clone();
            Box::pin(async move {
                match crate::db::notifications::cleanup_old_notifications(&db).await {
                    Ok(deleted) => {
                        if deleted > 0 {
                            tracing::info!(deleted, "notifications cleanup completed");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "notifications cleanup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Register a cron job that creates a backup on a configurable schedule.
    pub async fn add_backup(
        &self,
        cron_expr: &str,
        retention_days: u32,
        db: PgPool,
        secrets: Arc<crate::secrets::SecretsManager>,
        agent_deps: Arc<tokio::sync::RwLock<crate::gateway::state::AgentDeps>>,
    ) -> Result<()> {
        tracing::info!(cron = %cron_expr, retention_days, "scheduling automatic backup");

        let cron_expr = cron_expr.to_string();
        let job = Job::new_async(cron_expr.as_str(), move |_uuid, _lock| {
            let db = db.clone();
            let secrets = secrets.clone();
            let agent_deps = agent_deps.clone();
            Box::pin(async move {
                match crate::gateway::create_backup_internal(
                    &db,
                    &secrets,
                    &agent_deps,
                    i64::from(retention_days),
                ).await {
                    Ok(f) => tracing::info!(filename = %f, "scheduled backup created"),
                    Err(e) => tracing::error!(error = %e, "scheduled backup failed"),
                }
            })
        })?;

        self.scheduler.add(job).await?;
        Ok(())
    }

    /// Add a dynamic job from the database.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_dynamic_job(
        &self,
        db_id: Uuid,
        cron_expr: &str,
        timezone: &str,
        task_message: String,
        agent_name: String,
        engine: Arc<AgentEngine>,
        db: PgPool,
        announce_to: Option<serde_json::Value>,
        silent: bool,
        jitter_secs: i32,
        run_once: bool,
        run_at: Option<chrono::DateTime<chrono::Utc>>,
        tool_policy: Option<serde_json::Value>,
    ) -> Result<()> {
        // Normalize 5-field cron to 6-field by prepending "0 " for seconds
        let cron_6field = {
            let raw = cron_expr.trim();
            let fields: Vec<&str> = raw.split_whitespace().collect();
            if fields.len() == 5 {
                format!("0 {raw}")
            } else {
                raw.to_string()
            }
        };

        let cron_utc = convert_cron_to_utc(&cron_6field, timezone);

        // One-shot task: schedule via tokio::spawn instead of cron scheduler
        if run_once {
            let run_at = run_at.ok_or_else(|| anyhow::anyhow!("run_once job missing run_at"))?;
            let delay = (run_at - chrono::Utc::now())
                .to_std()
                .unwrap_or(std::time::Duration::ZERO);

            let fmt_prompt = engine.formatting_prompt().await;
            let msg = hydeclaw_types::IncomingMessage {
                user_id: "system".to_string(),
                text: Some(task_message),
                attachments: vec![],
                agent_id: agent_name.clone(),
                channel: crate::agent::channel_kind::channel::CRON.to_string(),
                context: announce_to.unwrap_or(serde_json::Value::Null),
                timestamp: chrono::Utc::now(),
                formatting_prompt: fmt_prompt,
                tool_policy_override: tool_policy.clone(),
                leaf_message_id: None,
            user_message_id: None,
            };

            let db2 = db.clone();
            let engine2 = engine.clone();
            let agent_name2 = agent_name.clone();
            let ui_tx = self.ui_event_tx.clone();

            tokio::spawn(async move {
                tokio::time::sleep(delay).await;

                let run_id: Option<uuid::Uuid> = match sqlx::query_scalar(
                    "INSERT INTO cron_runs (job_id, agent_id) VALUES ($1, $2) RETURNING id",
                )
                .bind(db_id)
                .bind(&*agent_name2)
                .fetch_optional(&db2)
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(agent = %agent_name2, job_id = %db_id, error = %e, "one-shot job: failed to insert cron_run");
                        None
                    }
                };

                match engine2.handle_isolated(&msg).await {
                    Ok(reply) => {
                        if let Some(rid) = run_id {
                            let preview = reply.chars().take(500).collect::<String>();
                            if let Err(e) = sqlx::query(
                                "UPDATE cron_runs SET status = 'success', finished_at = now(), \
                                 response_preview = $2 WHERE id = $1",
                            )
                            .bind(rid)
                            .bind(&preview)
                            .execute(&db2)
                            .await
                            {
                                tracing::warn!(agent = %agent_name2, job_id = %db_id, error = %e, "one-shot job: failed to record success");
                            }
                        }
                        tracing::info!(agent = %agent_name2, job_id = %db_id, "one-shot job completed");
                        broadcast_session_event(&ui_tx, &agent_name2, "cron");
                    }
                    Err(e) => {
                        if let Some(rid) = run_id
                            && let Err(db_err) = sqlx::query(
                                "UPDATE cron_runs SET status = 'error', finished_at = now(), \
                                 error = $2 WHERE id = $1",
                            )
                            .bind(rid)
                            .bind(format!("{e:#}"))
                            .execute(&db2)
                            .await
                            {
                                tracing::warn!(agent = %agent_name2, job_id = %db_id, error = %db_err, "one-shot job: failed to record error");
                            }
                        tracing::error!(agent = %agent_name2, job_id = %db_id, error = %e, "one-shot job failed");
                    }
                }

                // Auto-delete: CASCADE removes cron_runs too
                if let Err(e) = sqlx::query("DELETE FROM scheduled_jobs WHERE id = $1")
                    .bind(db_id)
                    .execute(&db2)
                    .await
                {
                    tracing::warn!(agent = %agent_name2, job_id = %db_id, error = %e, "one-shot job: failed to delete scheduled_job");
                }
            });
            return Ok(());
        }

        tracing::info!(
            db_id = %db_id,
            agent = %agent_name,
            cron = %cron_utc,
            "adding dynamic job"
        );

        let jitter_ms_max = jitter_secs as u64 * 1000;
        let db_id_clone = db_id;
        let ui_tx = self.ui_event_tx.clone();
        let locks = self.agent_locks.clone();
        let tool_policy_clone = tool_policy.clone();
        let job = Job::new_async(cron_utc.as_str(), move |_uuid, _lock| {
            let engine = engine.clone();
            let agent_name = agent_name.clone();
            let task_message = task_message.clone();
            let db = db.clone();
            let db_id = db_id_clone;
            let announce_to = announce_to.clone();
            let ui_tx = ui_tx.clone();
            let locks = locks.clone();
            let tool_policy = tool_policy_clone.clone();
            Box::pin(async move {
                if jitter_ms_max > 0 {
                    use rand::Rng;
                    let delay_ms = rand::rng().random_range(0u64..jitter_ms_max);
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                // Per-agent lock: skip if already running a scheduled task
                {
                    let mut active = locks.lock().await;
                    if active.contains(&agent_name) {
                        tracing::debug!(agent = %agent_name, job_id = %db_id, "cron skipped — agent busy with another scheduled task");
                        return;
                    }
                    active.insert(agent_name.clone());
                }
                tracing::info!(agent = %agent_name, job_id = %db_id, "dynamic job triggered");
                // Use the channel's formatting prompt cached on the engine (from last adapter connection).
                // This ensures cron output follows the same formatting rules as live chat.
                let fmt_prompt = engine.formatting_prompt().await;

                let msg = hydeclaw_types::IncomingMessage {
                    user_id: "system".to_string(),
                    text: Some(task_message),
                    attachments: vec![],
                    agent_id: agent_name.clone(),
                    channel: crate::agent::channel_kind::channel::CRON.to_string(),
                    context: announce_to.clone().unwrap_or(serde_json::Value::Null),
                    timestamp: chrono::Utc::now(),
                    formatting_prompt: fmt_prompt,
                    tool_policy_override: tool_policy.clone(),
                    leaf_message_id: None,
            user_message_id: None,
                };

                // Record cron run start
                let run_id: Option<uuid::Uuid> = match sqlx::query_scalar(
                    "INSERT INTO cron_runs (job_id, agent_id) VALUES ($1, $2) RETURNING id",
                )
                .bind(db_id)
                .bind(&*agent_name)
                .fetch_optional(&db)
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(agent = %agent_name, job_id = %db_id, error = %e, "dynamic job: failed to insert cron_run");
                        None
                    }
                };

                // Wrap execution in AssertUnwindSafe + catch_unwind so the agent lock
                // is always released even if handle_isolated panics.
                let exec_result = std::panic::AssertUnwindSafe(async {
                    match engine.handle_isolated(&msg).await {
                        Ok(reply) => {
                            // Update last_run_at
                            if let Err(e) = sqlx::query("UPDATE scheduled_jobs SET last_run_at = now() WHERE id = $1")
                                .bind(db_id)
                                .execute(&db)
                                .await
                            {
                                tracing::warn!(agent = %agent_name, job_id = %db_id, error = %e, "dynamic job: failed to update last_run_at");
                            }
                            // Record success
                            if let Some(rid) = run_id {
                                let preview = reply.chars().take(500).collect::<String>();
                                if let Err(e) = sqlx::query(
                                    "UPDATE cron_runs SET status = 'success', finished_at = now(), \
                                     response_preview = $2 WHERE id = $1",
                                )
                                .bind(rid)
                                .bind(&preview)
                                .execute(&db)
                                .await
                                {
                                    tracing::warn!(agent = %agent_name, job_id = %db_id, error = %e, "dynamic job: failed to record success");
                                }
                            }
                            tracing::info!(agent = %agent_name, job_id = %db_id, "dynamic job completed");

                            // Notify UI about new session
                            broadcast_session_event(&ui_tx, &agent_name, "cron");

                            // Announce result to channel unless job is silent.
                            // Silent jobs rely on the agent calling send_message explicitly when needed.
                            if !silent
                                && let Some(ref at) = announce_to
                                    && let (Some(ch), Some(cid)) = (
                                        at["channel"].as_str(),
                                        at["chat_id"].as_i64(),
                                    ) {
                                        let text = format!("⏰ *{}*\n\n{}",
                                            agent_name,
                                            reply.chars().take(2000).collect::<String>());
                                        if let Err(e) = engine.send_channel_message(ch, cid, &text).await {
                                            tracing::warn!(error = %e, "cron announce failed");
                                        }
                                    }
                        }
                        Err(e) => {
                            // Record error
                            if let Some(rid) = run_id
                                && let Err(db_err) = sqlx::query(
                                    "UPDATE cron_runs SET status = 'error', finished_at = now(), \
                                     error = $2 WHERE id = $1",
                                )
                                .bind(rid)
                                .bind(format!("{e:#}"))
                                .execute(&db)
                                .await
                                {
                                    tracing::warn!(agent = %agent_name, job_id = %db_id, error = %db_err, "dynamic job: failed to record error");
                                }
                            tracing::error!(agent = %agent_name, job_id = %db_id, error = %e, "dynamic job failed");
                        }
                    }
                });

                if let Err(panic_err) = futures_util::FutureExt::catch_unwind(exec_result).await {
                    let panic_msg = panic_err
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| panic_err.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    tracing::error!(agent = %agent_name, job_id = %db_id, error = %panic_msg, "dynamic job panicked");
                }
                // Always release the per-agent lock, even after panic
                locks.lock().await.remove(&agent_name);
            })
        })?;

        let scheduler_uuid = self.scheduler.add(job).await?;
        self.dynamic_jobs.write().await.insert(db_id, scheduler_uuid);
        Ok(())
    }

    /// Remove a dynamic job by its DB id.
    pub async fn remove_dynamic_job(&self, db_id: Uuid) -> Result<()> {
        let scheduler_uuid = self
            .dynamic_jobs
            .write()
            .await
            .remove(&db_id)
            .ok_or_else(|| anyhow::anyhow!("job {db_id} not found in scheduler"))?;

        self.scheduler.remove(&scheduler_uuid).await?;
        tracing::info!(db_id = %db_id, "dynamic job removed");
        Ok(())
    }

    /// Load all dynamic jobs from the database and schedule them.
    pub async fn load_dynamic_jobs(
        &self,
        db: &PgPool,
        engines: &std::collections::HashMap<String, Arc<AgentEngine>>,
    ) -> Result<usize> {
        let rows = sqlx::query_as::<_, (Uuid, String, String, String, String, Option<serde_json::Value>, bool, i32, bool, Option<chrono::DateTime<chrono::Utc>>, Option<serde_json::Value>)>(
            "SELECT id, agent_id, cron_expr, timezone, task_message, announce_to, silent, jitter_secs, run_once, run_at, tool_policy FROM scheduled_jobs WHERE enabled = true AND run_once = false",
        )
        .fetch_all(db)
        .await?;

        let mut count = 0;
        for (id, agent_id, cron_expr, timezone, task_message, announce_to, silent, jitter_secs, run_once, run_at, tool_policy) in rows {
            if let Some(engine) = engines.get(&agent_id) {
                match self
                    .add_dynamic_job(
                        id,
                        &cron_expr,
                        &timezone,
                        task_message,
                        agent_id.clone(),
                        engine.clone(),
                        db.clone(),
                        announce_to,
                        silent,
                        jitter_secs,
                        run_once,
                        run_at,
                        tool_policy,
                    )
                    .await
                {
                    Ok(()) => count += 1,
                    Err(e) => {
                        tracing::warn!(job_id = %id, agent = %agent_id, error = %e, "failed to load dynamic job");
                    }
                }
            } else {
                tracing::warn!(job_id = %id, agent = %agent_id, "no engine for agent, skipping job");
            }
        }

        if count > 0 {
            tracing::info!(count, "loaded dynamic jobs from database");
        }

        // Recover pending one-shot tasks after restart
        let once_rows = sqlx::query_as::<_, ScheduledJob>(
            "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, \
             last_run_at, silent, announce_to, jitter_secs, run_once, run_at, tool_policy \
             FROM scheduled_jobs WHERE run_once = true AND run_at > now()",
        )
        .fetch_all(db)
        .await?;

        for job in once_rows {
            if let Some(engine) = engines.get(&job.agent_id) {
                tracing::info!(job_id = %job.id, run_at = ?job.run_at, "recovering pending one-shot task");
                match self.add_dynamic_job(
                    job.id, &job.cron_expr, &job.timezone,
                    job.task_message.clone(), job.agent_id.clone(),
                    engine.clone(), db.clone(), job.announce_to.clone(),
                    job.silent, job.jitter_secs, job.run_once, job.run_at,
                    job.tool_policy.clone(),
                ).await {
                    Ok(()) => { count += 1; },
                    Err(e) => tracing::warn!(job_id = %job.id, error = %e, "failed to recover one-shot task"),
                }
            }
        }

        Ok(count)
    }

    /// List dynamic jobs. If `agent_id` is Some, filter by agent; if None, return all.
    pub async fn list_jobs(db: &PgPool, agent_id: Option<&str>) -> Result<Vec<ScheduledJob>> {
        let rows = match agent_id {
            Some(id) => {
                sqlx::query_as::<_, ScheduledJob>(
                    "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, last_run_at, silent, announce_to, jitter_secs, run_once, run_at, tool_policy \
                     FROM scheduled_jobs WHERE agent_id = $1 ORDER BY created_at DESC",
                )
                .bind(id)
                .fetch_all(db)
                .await?
            }
            None => {
                sqlx::query_as::<_, ScheduledJob>(
                    "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, last_run_at, silent, announce_to, jitter_secs, run_once, run_at, tool_policy \
                     FROM scheduled_jobs ORDER BY created_at DESC",
                )
                .fetch_all(db)
                .await?
            }
        };
        Ok(rows)
    }

    /// Remove a single job by scheduler UUID.
    pub async fn remove_job(&self, uuid: Uuid) -> Result<()> {
        self.scheduler.remove(&uuid).await?;
        Ok(())
    }

    /// Remove all agent-specific jobs (heartbeat) by agent name.
    #[allow(dead_code)]
    pub async fn remove_agent_jobs(&self, agent_name: &str) -> Result<()> {
        let uuids = self.agent_jobs.write().await.remove(agent_name);
        if let Some(uuids) = uuids {
            for uuid in uuids {
                if let Err(e) = self.scheduler.remove(&uuid).await {
                    tracing::warn!(agent = %agent_name, job = %uuid, error = %e, "failed to remove agent job");
                }
            }
        }
        Ok(())
    }

    /// Start the scheduler.
    pub async fn start(&self) -> Result<()> {
        self.scheduler.start().await?;
        tracing::info!("scheduler started");
        Ok(())
    }
}

/// Execute a heartbeat: read HEARTBEAT.md, send to agent engine, relay response to channel owner.
async fn run_heartbeat(
    engine: &AgentEngine,
    workspace_dir: &str,
    agent_name: &str,
    announce_channel: Option<&str>,
    owner_id: Option<&str>,
) -> Result<()> {
    let heartbeat_path = std::path::Path::new(workspace_dir)
        .join("agents")
        .join(agent_name)
        .join("HEARTBEAT.md");

    let checklist = tokio::fs::read_to_string(&heartbeat_path)
        .await
        .unwrap_or_else(|_| "No heartbeat checklist found.".to_string());

    let fmt_prompt = engine.formatting_prompt().await;

    // Build context from announce settings so agent's `message` tool can reach the owner.
    let context = match (announce_channel, owner_id.and_then(|s| s.parse::<i64>().ok())) {
        (Some(ch), Some(cid)) => serde_json::json!({ "channel": ch, "chat_id": cid }),
        _ => serde_json::Value::Null,
    };

    let msg = hydeclaw_types::IncomingMessage {
        user_id: "system".to_string(),
        text: Some(format!(
            "[Heartbeat] Complete the tasks from the checklist:\n\n{checklist}"
        )),
        attachments: vec![],
        agent_id: agent_name.to_string(),
        channel: crate::agent::channel_kind::channel::HEARTBEAT.to_string(),
        context,
        timestamp: chrono::Utc::now(),
        formatting_prompt: fmt_prompt,
        tool_policy_override: None,
        leaf_message_id: None,
        user_message_id: None,
    };

    let response = engine.handle_isolated(&msg).await?;

    // Suppress announcement when agent reports nothing to do
    let suppress = response.trim().eq_ignore_ascii_case(HEARTBEAT_OK);

    if suppress {
        tracing::info!(agent = %agent_name, "heartbeat OK — nothing to announce");
    } else {
        // Announce heartbeat result to channel (e.g. Telegram DM to owner)
        if let (Some(channel), Some(owner_str)) = (announce_channel, owner_id)
            && let Ok(chat_id) = owner_str.parse::<i64>() {
                let text = if response.len() > 3500 {
                    let boundary = response.floor_char_boundary(3500);
                    format!("{}...", &response[..boundary])
                } else {
                    response.clone()
                };
                if let Err(e) = engine.send_channel_message(channel, chat_id, &text).await {
                    tracing::warn!(agent = %agent_name, error = %e, "heartbeat announce failed");
                }
            }
    }

    // Skill evolution: analyze heartbeat for skill improvements (fire-and-forget)
    {
        let db = engine.db_pool().clone();
        let provider = engine.provider_arc();
        let agent = agent_name.to_string();
        let task = checklist.clone();
        let resp = response.clone();
        let was_ok = suppress;
        tokio::spawn(async move {
            crate::skills::evolution::analyze_and_evolve(
                &db, &provider, &agent, &task, &resp, &[], was_ok,
            ).await;
        });
    }

    tracing::info!(
        agent = %agent_name,
        response_len = response.len(),
        suppressed = suppress,
        "heartbeat completed"
    );

    Ok(())
}

/// Fire onboarding on first run (called when no sessions exist for the agent).
/// Sends a synthetic message to the engine, which greets the owner and collects setup info.
pub async fn run_first_run_onboarding(
    engine: &crate::agent::engine::AgentEngine,
    workspace_dir: &str,
    agent_name: &str,
) -> Result<()> {
    use crate::agent::channel_actions::ChannelAction;

    let agents_dir = std::path::Path::new(workspace_dir).join("agents").join(agent_name);

    let soul = tokio::fs::read_to_string(agents_dir.join("SOUL.md"))
        .await.unwrap_or_default();
    let identity = tokio::fs::read_to_string(agents_dir.join("IDENTITY.md"))
        .await.unwrap_or_default();
    let user_md = tokio::fs::read_to_string(
        std::path::Path::new(workspace_dir).join("USER.md"),
    ).await.unwrap_or_default();

    let msg = hydeclaw_types::IncomingMessage {
        user_id: "system".to_string(),
        text: Some(format!(
            "[FIRST RUN — agent: {agent_name}]\n\
            This is the first launch after a clean installation. \
            Your configuration files contain empty templates that need to be filled in.\n\n\
            Instructions:\n\
            1. Detect the owner's country and language from their Telegram profile locale or timezone \
               (check your available context). Default to the language most likely spoken in their region.\n\
            2. Send the owner a warm welcome message IN THEIR DETECTED LANGUAGE and ask them to tell you:\n\
               - Their name and how they prefer to be addressed\n\
               - Their timezone / city\n\
               - What they do (work, interests)\n\
               - How they want you to be: your name, personality, communication style\n\
            3. After receiving their answers, use workspace_write to update these EXACT paths:\n\
               - workspace/USER.md — shared user profile (name, timezone, preferences)\n\
               - workspace/agents/{agent_name}/IDENTITY.md — YOUR identity (name, role, style) — agent-specific!\n\
               - workspace/agents/{agent_name}/SOUL.md — YOUR character and values — agent-specific!\n\
            IMPORTANT: SOUL.md and IDENTITY.md must be written to workspace/agents/{agent_name}/ (not to workspace/ root).\n\n\
            Current templates (placeholders to replace):\n\
            workspace/agents/{agent_name}/SOUL.md:\n{soul}\n\n\
            workspace/agents/{agent_name}/IDENTITY.md:\n{identity}\n\n\
            workspace/USER.md:\n{user_md}"
        )),
        attachments: vec![],
        agent_id: agent_name.to_string(),
        channel: crate::agent::channel_kind::channel::SYSTEM.to_string(),
        context: serde_json::Value::Null,
        timestamp: chrono::Utc::now(),
        formatting_prompt: None,
        tool_policy_override: None,
        leaf_message_id: None,
        user_message_id: None,
    };

    let response = engine.handle_isolated(&msg).await?;

    if !response.is_empty()
        && let Some(router) = engine.channel_router_ref()
        && let Some(ac) = engine.agent_access()
        && let Some(ref owner_id) = ac.owner_id
    {
        let (reply_tx, _) = tokio::sync::oneshot::channel::<Result<(), String>>();
        router.send(ChannelAction {
            name: "send_message".to_string(),
            params: serde_json::json!({ "text": response }),
            context: serde_json::json!({ "owner_id": owner_id }),
            reply: reply_tx,
            target_channel: None, // first-run onboarding → send to any connected channel
        })
        .await
        .ok();
        tracing::info!(agent = %agent_name, "first-run onboarding message sent to channel owner");
    }

    Ok(())
}

/// Broadcast a `session_updated` event to UI via the shared broadcast channel.
fn broadcast_session_event(
    tx: &tokio::sync::broadcast::Sender<String>,
    agent: &str,
    channel: &str,
) {
    let event = serde_json::json!({
        "type": "session_updated",
        "agent": agent,
        "channel": channel,
    });
    tx.send(event.to_string()).ok();
}

/// Decay `relevance_score` for raw (non-pinned) memory chunks.
/// `half_life` = 30 days. Deletes chunks with score < 0.05.
async fn run_memory_decay(db: &PgPool) -> Result<(u64, u64)> {
    // Exponential decay: score *= exp(-0.693 / 30 * days_since_access)
    let decay_result = sqlx::query(
        "UPDATE memory_chunks \
         SET relevance_score = relevance_score * exp(-0.693 / 30.0 * \
             EXTRACT(EPOCH FROM (now() - accessed_at)) / 86400.0) \
         WHERE pinned = false \
           AND accessed_at < now() - interval '1 day'",
    )
    .execute(db)
    .await?;
    let decayed = decay_result.rows_affected();

    // Delete chunks with very low scores
    let delete_result = sqlx::query(
        "DELETE FROM memory_chunks \
         WHERE pinned = false AND relevance_score < 0.05",
    )
    .execute(db)
    .await?;
    let deleted = delete_result.rows_affected();

    Ok((decayed, deleted))
}

/// Delete old completed/failed tasks (>30 days) and orphan steps.
async fn run_task_cleanup(db: &PgPool) -> Result<(u64, u64)> {
    // Steps are cascade-deleted via FK, but count them first
    let steps_result = sqlx::query(
        "SELECT COUNT(*) as cnt FROM task_steps WHERE task_id IN \
         (SELECT id FROM tasks WHERE status IN ('completed', 'failed', 'cancelled') \
          AND updated_at < now() - interval '30 days')",
    )
    .fetch_one(db)
    .await?;
    let steps: i64 = sqlx::Row::get(&steps_result, "cnt");

    let tasks_result = sqlx::query(
        "DELETE FROM tasks \
         WHERE status IN ('completed', 'failed', 'cancelled') \
           AND updated_at < now() - interval '30 days'",
    )
    .execute(db)
    .await?;
    let tasks = tasks_result.rows_affected();

    Ok((tasks, steps as u64))
}

/// Compute the next fire time for a cron expression in the given timezone.
/// Returns RFC3339 string or None if the expression is invalid.
pub fn compute_next_run(cron_expr: &str, timezone: &str) -> Option<String> {
    use cron::Schedule;
    use std::str::FromStr;

    // Normalize to 6-field (sec min hour dom mon dow)
    let cron_6field = {
        let raw = cron_expr.trim();
        let fields: Vec<&str> = raw.split_whitespace().collect();
        if fields.len() == 5 {
            format!("0 {raw}")
        } else {
            raw.to_string()
        }
    };

    // Convert local timezone hours to UTC
    let cron_utc = convert_cron_to_utc(&cron_6field, timezone);

    // cron crate expects 7 fields (sec min hour dom mon dow year) — append year wildcard
    let cron_7field = format!("{cron_utc} *");

    let schedule = Schedule::from_str(&cron_7field).ok()?;
    let next = schedule.upcoming(chrono::Utc).next()?;

    // Convert back to local timezone for display
    let offset_hours = timezone_offset_hours(timezone);
    let local = next + chrono::Duration::hours(i64::from(offset_hours));
    Some(local.to_rfc3339())
}

/// Get UTC offset hours for common Russian timezones (no DST).
pub fn timezone_offset_hours(tz: &str) -> i32 {
    match tz {
        "Europe/Samara" => 4,
        "Europe/Moscow" => 3,
        "Europe/Kaliningrad" => 2,
        "Asia/Yekaterinburg" => 5,
        "Asia/Omsk" => 6,
        "Asia/Krasnoyarsk" => 7,
        "Asia/Irkutsk" => 8,
        "Asia/Yakutsk" => 9,
        "Asia/Vladivostok" => 10,
        "Asia/Magadan" => 11,
        "Asia/Kamchatka" => 12,
        _ => {
            tracing::warn!(timezone = %tz, "unknown timezone, using UTC");
            0
        }
    }
}

/// Convert cron hour fields from local timezone to UTC.
/// Input: 6-field cron (sec min hour dom mon dow).
pub fn convert_cron_to_utc(cron: &str, timezone: &str) -> String {
    let offset = timezone_offset_hours(timezone);
    if offset == 0 {
        return cron.to_string();
    }

    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 6 {
        return cron.to_string();
    }

    let hour_field = fields[2];

    let new_hour = if hour_field == "*" {
        "*".to_string()
    } else if hour_field.starts_with("*/") {
        // */N pattern — cannot shift, keep as-is (approximate; step patterns don't shift cleanly)
        hour_field.to_string()
    } else if hour_field.contains(',') {
        // Comma-separated hours: "10,12,14,16,18,20,22"
        hour_field
            .split(',')
            .map(|h| {
                h.trim()
                    .parse::<i32>().map_or_else(|_| h.trim().to_string(), |v| (v - offset).rem_euclid(24).to_string())
            })
            .collect::<Vec<_>>()
            .join(",")
    } else if let Some((start, end)) = hour_field.split_once('-') {
        if let (Ok(s), Ok(e)) = (start.parse::<i32>(), end.parse::<i32>()) {
            let s_utc = (s - offset).rem_euclid(24);
            let e_utc = (e - offset).rem_euclid(24);
            format!("{s_utc}-{e_utc}")
        } else {
            hour_field.to_string()
        }
    } else if let Ok(h) = hour_field.parse::<i32>() {
        let h_utc = (h - offset).rem_euclid(24);
        h_utc.to_string()
    } else {
        hour_field.to_string()
    };

    format!(
        "{} {} {} {} {} {}",
        fields[0], fields[1], new_hour, fields[3], fields[4], fields[5]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── timezone_offset_hours ──────────────────────────────────────────

    #[test]
    fn tz_offset_samara() {
        assert_eq!(timezone_offset_hours("Europe/Samara"), 4);
    }

    #[test]
    fn tz_offset_moscow() {
        assert_eq!(timezone_offset_hours("Europe/Moscow"), 3);
    }

    #[test]
    fn tz_offset_kaliningrad() {
        assert_eq!(timezone_offset_hours("Europe/Kaliningrad"), 2);
    }

    #[test]
    fn tz_offset_yekaterinburg() {
        assert_eq!(timezone_offset_hours("Asia/Yekaterinburg"), 5);
    }

    #[test]
    fn tz_offset_omsk() {
        assert_eq!(timezone_offset_hours("Asia/Omsk"), 6);
    }

    #[test]
    fn tz_offset_krasnoyarsk() {
        assert_eq!(timezone_offset_hours("Asia/Krasnoyarsk"), 7);
    }

    #[test]
    fn tz_offset_irkutsk() {
        assert_eq!(timezone_offset_hours("Asia/Irkutsk"), 8);
    }

    #[test]
    fn tz_offset_yakutsk() {
        assert_eq!(timezone_offset_hours("Asia/Yakutsk"), 9);
    }

    #[test]
    fn tz_offset_vladivostok() {
        assert_eq!(timezone_offset_hours("Asia/Vladivostok"), 10);
    }

    #[test]
    fn tz_offset_magadan() {
        assert_eq!(timezone_offset_hours("Asia/Magadan"), 11);
    }

    #[test]
    fn tz_offset_kamchatka() {
        assert_eq!(timezone_offset_hours("Asia/Kamchatka"), 12);
    }

    #[test]
    fn tz_offset_unknown_returns_zero() {
        assert_eq!(timezone_offset_hours("America/New_York"), 0);
        assert_eq!(timezone_offset_hours(""), 0);
        assert_eq!(timezone_offset_hours("UTC"), 0);
        assert_eq!(timezone_offset_hours("nonsense"), 0);
    }

    // ── convert_cron_to_utc ────────────────────────────────────────────

    #[test]
    fn cron_utc_single_hour_samara() {
        // Samara +4: local 10 → UTC 6
        let result = convert_cron_to_utc("0 0 10 * * *", "Europe/Samara");
        assert_eq!(result, "0 0 6 * * *");
    }

    #[test]
    fn cron_utc_single_hour_moscow() {
        // Moscow +3: local 10 → UTC 7
        let result = convert_cron_to_utc("0 0 10 * * *", "Europe/Moscow");
        assert_eq!(result, "0 0 7 * * *");
    }

    #[test]
    fn cron_utc_comma_separated_moscow() {
        // Moscow +3: 10,14,18 → 7,11,15
        let result = convert_cron_to_utc("0 0 10,14,18 * * *", "Europe/Moscow");
        assert_eq!(result, "0 0 7,11,15 * * *");
    }

    #[test]
    fn cron_utc_comma_separated_samara() {
        // Samara +4: 10,12,14,16,20,22 → 6,8,10,12,16,18
        let result = convert_cron_to_utc("0 0 10,12,14,16,20,22 * * *", "Europe/Samara");
        assert_eq!(result, "0 0 6,8,10,12,16,18 * * *");
    }

    #[test]
    fn cron_utc_range() {
        // Moscow +3: 9-17 → 6-14
        let result = convert_cron_to_utc("0 0 9-17 * * *", "Europe/Moscow");
        assert_eq!(result, "0 0 6-14 * * *");
    }

    #[test]
    fn cron_utc_wildcard_unchanged() {
        let result = convert_cron_to_utc("0 0 * * * *", "Europe/Samara");
        assert_eq!(result, "0 0 * * * *");
    }

    #[test]
    fn cron_utc_step_unchanged() {
        // */2 pattern cannot be shifted, stays as-is
        let result = convert_cron_to_utc("0 0 */2 * * *", "Europe/Samara");
        assert_eq!(result, "0 0 */2 * * *");
    }

    #[test]
    fn cron_utc_offset_zero_unchanged() {
        // Unknown timezone → offset 0 → no conversion
        let input = "0 30 10 * * *";
        let result = convert_cron_to_utc(input, "UTC");
        assert_eq!(result, input);
    }

    #[test]
    fn cron_utc_wrap_around_midnight() {
        // Samara +4: local hour 2 → UTC 22 (previous day wrap)
        let result = convert_cron_to_utc("0 0 2 * * *", "Europe/Samara");
        assert_eq!(result, "0 0 22 * * *");
    }

    #[test]
    fn cron_utc_wrap_hour_zero() {
        // Moscow +3: local 0 → UTC 21
        let result = convert_cron_to_utc("0 0 0 * * *", "Europe/Moscow");
        assert_eq!(result, "0 0 21 * * *");
    }

    #[test]
    fn cron_utc_wrap_hour_one() {
        // Moscow +3: local 1 → UTC 22
        let result = convert_cron_to_utc("0 0 1 * * *", "Europe/Moscow");
        assert_eq!(result, "0 0 22 * * *");
    }

    #[test]
    fn cron_utc_high_offset_kamchatka() {
        // Kamchatka +12: local 10 → UTC 22 (previous day)
        let result = convert_cron_to_utc("0 0 10 * * *", "Asia/Kamchatka");
        assert_eq!(result, "0 0 22 * * *");
    }

    #[test]
    fn cron_utc_preserves_other_fields() {
        // Ensure minute, dom, mon, dow fields are not touched
        let result = convert_cron_to_utc("0 30 15 1 6 3", "Europe/Moscow");
        assert_eq!(result, "0 30 12 1 6 3");
    }

    #[test]
    fn cron_utc_wrong_field_count_passthrough() {
        // Not 6 fields → returned as-is
        let input = "0 10 * * *";
        let result = convert_cron_to_utc(input, "Europe/Samara");
        assert_eq!(result, input);
    }

    #[test]
    fn cron_utc_comma_wrap_around() {
        // Samara +4: comma list with values that wrap: 1,3 → 21,23
        let result = convert_cron_to_utc("0 0 1,3 * * *", "Europe/Samara");
        assert_eq!(result, "0 0 21,23 * * *");
    }

    #[test]
    fn cron_utc_range_wrap_around() {
        // Samara +4: range 0-3 → 20-23
        let result = convert_cron_to_utc("0 0 0-3 * * *", "Europe/Samara");
        assert_eq!(result, "0 0 20-23 * * *");
    }

    // ── compute_next_run ───────────────────────────────────────────────

    #[test]
    fn compute_next_run_valid_cron() {
        // "* * * * *" fires every minute — should always produce a next run
        let result = compute_next_run("* * * * *", "UTC");
        assert!(result.is_some(), "expected Some for a valid cron");
        // The result should be a valid RFC3339 timestamp
        let ts = result.unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(&ts).is_ok(),
            "expected valid RFC3339, got: {}",
            ts
        );
    }

    #[test]
    fn compute_next_run_invalid_cron_returns_none() {
        let result = compute_next_run("not a cron", "UTC");
        assert!(result.is_none(), "expected None for invalid cron");
    }

    #[test]
    fn compute_next_run_with_timezone() {
        // Should succeed with a timezone and produce a valid timestamp
        let result = compute_next_run("0 10 * * *", "Europe/Samara");
        assert!(result.is_some(), "expected Some for valid cron with timezone");
        let ts = result.unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(&ts).is_ok(),
            "expected valid RFC3339, got: {}",
            ts
        );
    }

    #[test]
    fn compute_next_run_five_field_normalized() {
        // 5-field cron should be normalized to 6-field internally and still work
        let result = compute_next_run("30 14 * * *", "Europe/Moscow");
        assert!(result.is_some());
    }

    #[test]
    fn compute_next_run_six_field_also_works() {
        // 6-field cron passed directly
        let result = compute_next_run("0 30 14 * * *", "Europe/Moscow");
        assert!(result.is_some());
    }

    #[test]
    fn compute_next_run_future_timestamp() {
        let result = compute_next_run("* * * * *", "UTC").unwrap();
        let next = chrono::DateTime::parse_from_rfc3339(&result).unwrap();
        let now = chrono::Utc::now();
        assert!(
            next > now - chrono::Duration::seconds(2),
            "next run should be in the future (or within 2s tolerance)"
        );
    }

    #[test]
    fn scheduled_job_tool_policy_compile_check() {
        // Compile-time check that field exists
        let _ = |job: ScheduledJob| {
            let _: Option<serde_json::Value> = job.tool_policy;
        };
    }
}
