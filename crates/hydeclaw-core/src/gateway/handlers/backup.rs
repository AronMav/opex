use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::path::Path as FsPath;
use std::sync::Arc;
use struson::reader::{JsonReader, JsonStreamReader};
use tokio::fs;

use sqlx::Row;

use crate::gateway::AppState;
use crate::gateway::clusters::{AgentCore, AuthServices, ConfigServices, InfraServices};
use crate::gateway::restore_stream_core::{
    check_content_length_cap, drain_body_with_cap, CapExceeded,
};
use crate::secrets::{PlaintextSecret, SecretsManager};

pub(crate) fn routes() -> Router<AppState> {
    // Phase 64 SEC-04: `/api/restore` caps request body size via the per-handler
    // `cfg.config.limits.max_restore_size_mb` (default 500 MB), enforced by the
    // `check_content_length_cap` fast-path AND the `drain_body_with_cap` streaming
    // byte counter. We must `DefaultBodyLimit::disable()` here so axum's default
    // 2 MB extractor limit doesn't short-circuit our own cap check.
    Router::new()
        .route("/api/backup", post(api_create_backup).get(api_list_backups))
        .route("/api/backup/{filename}", get(api_download_backup).delete(api_delete_backup))
        .merge(
            Router::new()
                .route("/api/restore", post(api_restore))
                .layer(DefaultBodyLimit::disable()),
        )
}

const BACKUP_DIR: &str = "backups";
const RETENTION_DAYS: i64 = 7;

/// Tables excluded from pg_dump — ephemeral or too large to be useful in backups.
pub(crate) const EXCLUDED_TABLES: &[&str] = &[
    "sessions", "messages", "session_events",
    "usage_log",
    "audit_log", "audit_events",
    "notifications",
    "pending_approvals",
    "pending_messages", "outbound_queue",
    "memory_tasks",
    "pairing_codes",
    "cron_runs",
    "tool_execution_cache",
    "stream_jobs",
    "graph_extraction_queue",
    "tasks", "task_steps",
    "secrets",
];

/// Parse the first non-empty container name from `docker ps` stdout.
fn parse_container_name<'a>(docker_output: &'a str, fallback: &'a str) -> &'a str {
    docker_output
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or(fallback)
}

/// Discover the running postgres container name.
/// Tries `docker ps --filter name=postgres`; falls back to `configured`.
async fn discover_postgres_container(configured: &str) -> String {
    let out = tokio::process::Command::new("docker")
        .args(["ps", "--filter", "name=postgres", "--format", "{{.Names}}"])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            parse_container_name(&stdout, configured).to_owned()
        }
        _ => configured.to_owned(),
    }
}

/// Run pg_dump inside the postgres container and write output to `dest`.
async fn run_pg_dump(container: &str, dest: &std::path::Path) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let file = tokio::fs::File::create(dest).await
        .with_context(|| format!("create db.dump at {}", dest.display()))?;

    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(["exec", container, "pg_dump", "-U", "hydeclaw", "hydeclaw", "-Fc"]);
    for table in EXCLUDED_TABLES {
        cmd.args(["--exclude-table", table]);
    }
    cmd.stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("docker exec pg_dump: spawn failed")?;

    // Copy pg_dump stdout → file without buffering in RAM
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut writer = tokio::io::BufWriter::new(file);
    tokio::io::copy(&mut stdout, &mut writer).await
        .context("streaming pg_dump output to db.dump")?;
    writer.flush().await.context("flush db.dump")?;

    let output = child.wait_with_output().await.context("pg_dump wait")?;
    if !output.status.success() {
        anyhow::bail!("pg_dump failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// Get the PostgreSQL version string from inside the container for the manifest.
async fn get_pg_version(container: &str) -> anyhow::Result<String> {
    let out = tokio::process::Command::new("docker")
        .args(["exec", container, "psql", "-U", "hydeclaw", "-t", "-c", "SELECT version()"])
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Copy directory contents to `dst` using `cp -r src/. dst/`.
/// Creates `dst` if it does not exist.
async fn copy_dir_to(src: &str, dst: &std::path::Path) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let src_dot = format!("{src}/.");
    let out = tokio::process::Command::new("cp")
        .args(["-r", &src_dot])
        .arg(dst)
        .output()
        .await
        .with_context(|| format!("cp -r {src}/. failed"))?;
    if !out.status.success() {
        anyhow::bail!("cp failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Run pg_restore inside the postgres container, reading db.dump from stdin.
async fn run_pg_restore(container: &str, dump_path: &std::path::Path) -> anyhow::Result<()> {
    // Open the dump file as a std::fs::File so it can be used as Stdio::from
    let file = std::fs::File::open(dump_path)
        .with_context(|| format!("open db.dump at {}", dump_path.display()))?;

    let out = tokio::process::Command::new("docker")
        .args([
            "exec", "-i", container,
            "pg_restore", "-U", "hydeclaw", "-d", "hydeclaw",
            "--clean", "--if-exists", "--exit-on-error", "-Fc",
        ])
        .stdin(std::process::Stdio::from(file))
        .output()
        .await
        .context("docker exec pg_restore: spawn failed")?;

    if !out.status.success() {
        anyhow::bail!("pg_restore failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Load agent configs from disk and restart all agents.
async fn restart_agents_from_disk(
    agents: &crate::gateway::clusters::AgentCore,
    infra: &crate::gateway::clusters::InfraServices,
    auth: &crate::gateway::clusters::AuthServices,
    bus: &crate::gateway::clusters::ChannelBus,
    cfg_svc: &crate::gateway::clusters::ConfigServices,
    status: &crate::gateway::clusters::StatusMonitor,
) -> Vec<String> {
    let Ok(agent_configs) = crate::config::load_agent_configs("config/agents") else {
        return vec![];
    };
    let mut restarted = Vec::new();
    for cfg in &agent_configs {
        match super::agents::start_agent_from_config(
            cfg, agents, infra, auth, bus, cfg_svc, status
        ).await {
            Ok((handle, guard)) => {
                let name = cfg.agent.name.clone();
                agents.map.write().await.insert(name.clone(), handle);
                if let Some(g) = guard {
                    auth.access_guards.write().await.insert(name.clone(), g);
                }
                restarted.push(name);
            }
            Err(e) => {
                tracing::error!(agent = %cfg.agent.name, %e, "restart failed after restore");
            }
        }
    }
    restarted
}

include!("backup_dto_structs.rs");

// ── Backup file format ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupFile {
    pub version: u32,
    pub created_at: chrono::DateTime<Utc>,
    pub config: Value,
    pub workspace: Vec<WorkspaceFile>,
    pub secrets: Vec<PlaintextSecret>,
    pub memory: Vec<MemoryChunk>,
    pub cron: Vec<CronJob>,
    #[serde(default)]
    pub providers: Vec<BackupProvider>,
    #[serde(default)]
    pub provider_active: Vec<BackupProviderActive>,
    #[serde(default)]
    pub channels: Vec<BackupChannel>,
    #[serde(default)]
    pub webhooks: Vec<BackupWebhook>,
    #[serde(default)]
    pub watchdog_settings: Vec<BackupWatchdogSetting>,
    #[serde(default)]
    pub allowed_users: Vec<BackupAllowedUser>,
    #[serde(default)]
    pub approval_allowlist: Vec<BackupApprovalAllow>,
    #[serde(default)]
    pub oauth_accounts: Vec<BackupOAuthAccount>,
    #[serde(default)]
    pub oauth_bindings: Vec<BackupOAuthBinding>,
    #[serde(default)]
    pub gmail_triggers: Vec<BackupGmailTrigger>,
    #[serde(default)]
    pub github_repos: Vec<BackupGithubRepo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct MemoryChunk {
    pub id: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    pub content: String,
    pub source: Option<String>,
    pub pinned: bool,
    pub relevance_score: f64,
    pub created_at: chrono::DateTime<Utc>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub chunk_index: i32,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CronJob {
    pub agent_id: String,
    pub name: String,
    pub cron_expr: String,
    pub timezone: String,
    pub task_message: String,
    pub enabled: bool,
    pub announce_to: Option<Value>,
    pub silent: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupProvider {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub category: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub enabled: bool,
    pub options: Value,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupProviderActive {
    pub capability: String,
    pub provider_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupChannel {
    pub id: String,
    pub agent_name: String,
    pub channel_type: String,
    pub display_name: String,
    pub config: Value,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupWebhook {
    pub name: String,
    pub agent_id: String,
    pub secret: Option<String>,
    pub prompt_prefix: Option<String>,
    pub enabled: bool,
    pub webhook_type: String,
    pub event_filter: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupWatchdogSetting {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupAllowedUser {
    pub agent_id: String,
    pub channel_user_id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupApprovalAllow {
    pub agent_id: String,
    pub tool_pattern: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupOAuthAccount {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    pub scope: String,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupOAuthBinding {
    pub agent_id: String,
    pub provider: String,
    pub account_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupGmailTrigger {
    pub agent_id: String,
    pub email_address: String,
    pub pubsub_topic: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BackupGithubRepo {
    pub agent_id: String,
    pub owner: String,
    pub repo: String,
}

// ── POST /api/backup ─────────────────────────────────────────────────────────

/// Create a pg_dump-based backup bundle (.tar.gz).
pub(crate) async fn create_backup_internal(
    secrets: &Arc<crate::secrets::SecretsManager>,
    agent_deps: &Arc<tokio::sync::RwLock<crate::gateway::state::AgentDeps>>,
    retention_days: i64,
    postgres_container: &str,
) -> anyhow::Result<String> {
    let now = chrono::Utc::now();
    let filename = format!("hydeclaw-{}.tar.gz", now.format("%Y-%m-%d"));

    let container = discover_postgres_container(postgres_container).await;

    // Temp dir with restricted permissions
    let tmpdir = std::env::temp_dir()
        .join(format!("hydeclaw-backup-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&tmpdir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmpdir,
            std::fs::Permissions::from_mode(0o700)).await?;
    }

    let result: anyhow::Result<String> = async {
        // 1. pg_dump
        run_pg_dump(&container, &tmpdir.join("db.dump")).await?;

        // 2. Secrets — plaintext, chmod 600
        let backup_secrets = secrets.export_decrypted().await
            .context("secrets export failed")?;
        let secrets_path = tmpdir.join("secrets.json");
        tokio::fs::write(&secrets_path,
            serde_json::to_vec(&backup_secrets).context("secrets serialize")?)
            .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&secrets_path,
                std::fs::Permissions::from_mode(0o600)).await?;
        }

        // 3. Manifest
        let pg_version = get_pg_version(&container).await.unwrap_or_default();
        tokio::fs::write(
            tmpdir.join("manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 3,
                "created_at": now,
                "pg_version": pg_version,
                "excluded_tables": EXCLUDED_TABLES,
            })).context("manifest serialize")?,
        ).await?;

        // 4. workspace/ and config/
        let workspace_dir = {
            let deps = agent_deps.read().await;
            deps.workspace_dir.clone()
        };
        copy_dir_to(&workspace_dir, &tmpdir.join("workspace")).await?;
        copy_dir_to("config", &tmpdir.join("config")).await?;

        // 5. tar czf
        tokio::fs::create_dir_all(BACKUP_DIR).await?;
        let filepath = format!("{BACKUP_DIR}/{filename}");
        let tar_out = tokio::process::Command::new("tar")
            .args(["czf", &filepath, "-C"])
            .arg(&tmpdir)
            .arg(".")
            .output().await
            .context("tar czf: spawn failed")?;
        if !tar_out.status.success() {
            anyhow::bail!("tar failed: {}", String::from_utf8_lossy(&tar_out.stderr));
        }

        Ok(filename.clone())
    }.await;

    // Always clean up temp dir, even on error
    let _ = tokio::fs::remove_dir_all(&tmpdir).await;

    let filename = result?;

    // Cleanup old backups
    cleanup_old_backups_v3(now, retention_days).await;

    let size = tokio::fs::metadata(format!("{BACKUP_DIR}/{filename}"))
        .await.map(|m| m.len()).unwrap_or(0);
    tracing::info!(filename = %filename, size_bytes = size, "backup created");
    Ok(filename)
}

async fn cleanup_old_backups_v3(now: chrono::DateTime<chrono::Utc>, retention_days: i64) {
    if retention_days == 0 { return; }  // 0 = disabled
    let cutoff = now - chrono::Duration::days(retention_days);
    let Ok(mut dir) = tokio::fs::read_dir(BACKUP_DIR).await else { return };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        if !name.ends_with(".tar.gz") { continue; }
        if let Some(date_part) = name.strip_prefix("hydeclaw-").and_then(|s| s.strip_suffix(".tar.gz"))
            && let Ok(date) = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d")
        {
            let file_dt = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
            if file_dt < cutoff {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }
}

/// Create a backup without Axum context — callable from the scheduler or HTTP handler.
/// Returns the filename of the created backup on success.
async fn create_backup_internal_legacy(
    db: &PgPool,
    secrets: &Arc<SecretsManager>,
    agent_deps: &Arc<tokio::sync::RwLock<crate::gateway::state::AgentDeps>>,
    retention_days: i64,
) -> Result<String> {
    let now = Utc::now();
    let date_str = now.format("%Y-%m-%d").to_string();
    let filename = format!("hydeclaw-{date_str}.json");

    // 1. Config
    let app_toml = std::fs::read_to_string("config/hydeclaw.toml").unwrap_or_default();
    let mut agents = serde_json::Map::new();
    if let Ok(entries) = std::fs::read_dir("config/agents") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                agents.insert(name, Value::String(content));
            }
        }
    }
    let config = json!({ "app_config": app_toml, "agents": agents });

    // 2. Workspace files (recursive walk, text only)
    let workspace_dir = {
        let deps = agent_deps.read().await;
        deps.workspace_dir.clone()
    };
    let workspace = collect_workspace_files(&workspace_dir).await;

    // 3. Secrets (raw encrypted blobs)
    let backup_secrets = secrets.export_decrypted().await
        .map_err(|e| anyhow::anyhow!("secrets export failed: {e}"))?;

    // 4. Memory chunks (no embeddings)
    let memory = collect_memory_from_db(db).await
        .map_err(|e| anyhow::anyhow!("memory collection failed: {e}"))?;

    // 5. Cron jobs
    let cron = collect_cron_from_db(db).await
        .map_err(|e| anyhow::anyhow!("cron collection failed: {e}"))?;

    // 6. V2 sections (non-fatal — default to empty on error)
    tracing::info!("backup: collecting V2 sections...");
    let providers = collect_providers(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: providers failed"); vec![] });
    tracing::info!(count = providers.len(), "backup: providers");
    let provider_active = collect_provider_active(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: provider_active failed"); vec![] });
    tracing::info!(count = provider_active.len(), "backup: provider_active");
    let channels = collect_channels(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: channels failed"); vec![] });
    tracing::info!(count = channels.len(), "backup: channels");
    let webhooks = collect_webhooks(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: webhooks failed"); vec![] });
    tracing::info!(count = webhooks.len(), "backup: webhooks");
    let watchdog_settings = collect_watchdog_settings(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: watchdog failed"); vec![] });
    tracing::info!(count = watchdog_settings.len(), "backup: watchdog_settings");
    let allowed_users = collect_allowed_users(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: allowed_users failed"); vec![] });
    tracing::info!(count = allowed_users.len(), "backup: allowed_users");
    let approval_allowlist = collect_approval_allowlist(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: approval failed"); vec![] });
    tracing::info!(count = approval_allowlist.len(), "backup: approval_allowlist");
    let oauth_accounts = collect_oauth_accounts(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: oauth_accounts failed"); vec![] });
    let oauth_bindings = collect_oauth_bindings(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: oauth_bindings failed"); vec![] });
    let gmail_triggers = collect_gmail_triggers(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: gmail_triggers failed"); vec![] });
    let github_repos = collect_github_repos(db).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "backup: github_repos failed"); vec![] });

    let backup = BackupFile {
        version: 2,
        created_at: now,
        config,
        workspace,
        secrets: backup_secrets,
        memory,
        cron,
        providers,
        provider_active,
        channels,
        webhooks,
        watchdog_settings,
        allowed_users,
        approval_allowlist,
        oauth_accounts,
        oauth_bindings,
        gmail_triggers,
        github_repos,
    };

    // Serialize to JSON
    let json_bytes = serde_json::to_vec_pretty(&backup)
        .map_err(|e| anyhow::anyhow!("serialization failed: {e}"))?;

    // Save to disk
    fs::create_dir_all(BACKUP_DIR).await
        .map_err(|e| anyhow::anyhow!("cannot create backup dir: {e}"))?;
    let filepath = format!("{BACKUP_DIR}/{filename}");
    fs::write(&filepath, &json_bytes).await
        .map_err(|e| anyhow::anyhow!("cannot write backup: {e}"))?;

    // Cleanup old backups
    cleanup_old_backups_with_retention(now, retention_days).await;

    let size_bytes = json_bytes.len();
    tracing::info!(filename = %filename, size_bytes = size_bytes, "backup created");

    Ok(filename)
}

/// Create a backup: pg_dump + workspace + config + secrets, bundle as .tar.gz.
pub(crate) async fn api_create_backup(
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    State(cfg_svc): State<ConfigServices>,
) -> impl IntoResponse {
    let now = chrono::Utc::now();
    let retention = cfg_svc.config.backup.retention_days as i64;
    let container = cfg_svc.config.backup.postgres_container.clone();
    match create_backup_internal(&auth.secrets, &agents.deps, retention, &container).await {
        Ok(filename) => {
            let filepath = format!("{BACKUP_DIR}/{filename}");
            Json(json!({
                "ok": true,
                "filename": filename,
                "path": filepath,
                "created_at": now,
            })).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

// ── GET /api/backup ──────────────────────────────────────────────────────────

pub(crate) async fn api_list_backups() -> impl IntoResponse {
    let mut entries: Vec<BackupEntryDto> = Vec::new();
    if let Ok(mut dir) = fs::read_dir(BACKUP_DIR).await {
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            if !name.ends_with(".tar.gz") { continue; }
            let filename = name.clone();
            if let Ok(meta) = entry.metadata().await {
                let size_bytes = meta.len();
                let created_at = name
                    .strip_prefix("hydeclaw-")
                    .and_then(|s| s.strip_suffix(".tar.gz"))
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339())
                    .or_else(|| {
                        meta.modified().ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .and_then(|d| chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0))
                            .map(|dt| dt.to_rfc3339())
                    });
                entries.push(BackupEntryDto { filename, size_bytes, created_at });
            }
        }
    }
    entries.sort_by(|a, b| b.filename.cmp(&a.filename));
    Json(serde_json::json!({ "backups": entries }))
}

// ── GET /api/backup/:filename ─────────────────────────────────────────────────

pub(crate) async fn api_download_backup(Path(filename): Path<String>) -> impl IntoResponse {
    // Prevent path traversal
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") || filename.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid filename"}))).into_response();
    }
    let filepath = format!("{BACKUP_DIR}/{filename}");
    match fs::read(&filepath).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/gzip".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
            ],
            bytes,
        ).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "backup not found"}))).into_response(),
    }
}

// ── DELETE /api/backup/:filename ─────────────────────────────────────────────

pub(crate) async fn api_delete_backup(Path(filename): Path<String>) -> impl IntoResponse {
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") || filename.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid filename"}))).into_response();
    }
    let filepath = format!("{BACKUP_DIR}/{filename}");
    match fs::remove_file(&filepath).await {
        Ok(()) => {
            tracing::info!(filename = %filename, "backup deleted via API");
            Json(json!({"ok": true})).into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "backup not found"}))).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

// ── POST /api/restore ─────────────────────────────────────────────────────────

/// Phase 64 SEC-04 — streaming restore handler (legacy JSON format v1/v2).
///
/// Replaces the old `Json<BackupFile>` extractor with a bounded streaming pipeline:
///   1. Validate the `X-Confirm-Restore` header (unchanged security gate).
///   2. `check_content_length_cap` fast-path — 413 in <1ms if CL exceeds cap.
///   3. `drain_body_with_cap` — streams the body, aborts at exact cap if CL was
///      missing or lying. Returns a bounded `Vec<u8>` buffer.
///   4. `parse_backup_stream` — struson JsonStreamReader section walk. NO
///      `serde_json::from_slice(&buf)` fallback (CONTEXT D-SEC-04).
#[allow(clippy::too_many_arguments)]
async fn api_restore_legacy(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    State(bus): State<crate::gateway::clusters::ChannelBus>,
    State(cfg_svc): State<crate::gateway::clusters::ConfigServices>,
    State(status): State<crate::gateway::clusters::StatusMonitor>,
    req: Request,
) -> axum::response::Response {
    // Security: restore is a destructive operation — require X-Confirm-Restore header
    // to prevent accidental or automated restore via stolen API token.
    let headers = req.headers().clone();
    let confirm = headers.get("x-confirm-restore").and_then(|v| v.to_str().ok()).unwrap_or("");
    if confirm != "yes-i-am-sure" {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "restore requires X-Confirm-Restore: yes-i-am-sure header"
        }))).into_response();
    }

    // Phase 64 SEC-04: Content-Length fast-path. Rejects oversized uploads with a
    // 413 + structured JSON body in <1ms — before we touch the body stream.
    let cap_mb = cfg_svc.config.limits.max_restore_size_mb;
    let cap_bytes = (cap_mb as usize).saturating_mul(1024 * 1024);
    if let Some((status, body)) = check_content_length_cap(&headers, cap_bytes) {
        tracing::warn!(
            cap_mb,
            "restore rejected via Content-Length fast-path (payload > cap)"
        );
        return (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response();
    }

    // Streaming body drain with byte-counter cap enforcement. Catches the case where
    // the client omitted Content-Length or lied about it; aborts the moment cumulative
    // bytes cross the cap. NOTE: `into_data_stream()` yields axum::Error on I/O errors
    // — `drain_body_with_cap` maps those to CapExceeded (safer default than silent pass).
    let body = req.into_body();
    let stream = body.into_data_stream();
    let buf = match drain_body_with_cap(stream, cap_bytes).await {
        Ok(b) => b,
        Err(CapExceeded { observed_bytes, cap_bytes }) => {
            tracing::warn!(
                observed_bytes,
                cap_bytes,
                "restore rejected via streaming byte-counter (payload > cap or body error)"
            );
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": "payload exceeds max_restore_size_mb",
                    "cap_bytes": cap_bytes,
                    "observed_bytes": observed_bytes,
                })),
            )
                .into_response();
        }
    };

    // Delegate to the shared JSON restore core logic.
    restore_from_json_buf(buf, infra, auth, agents, bus, cfg_svc, status).await
}

// ── POST /api/restore (v3 tar.gz / pg_dump format) ────────────────────────────

/// Restore from a v3 `.tar.gz` backup produced by `api_create_backup`.
///
/// The archive must contain:
///   - `manifest.json`  — must have a `"version"` field
///   - `db.dump`        — custom-format pg_dump (restored via pg_restore)
///   - `secrets.json`   — plaintext secrets array
///   - `workspace/`     — workspace directory tree (optional)
///   - `config/`        — agent configs (optional)
#[allow(clippy::too_many_arguments)]
pub(crate) async fn api_restore(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    State(bus): State<crate::gateway::clusters::ChannelBus>,
    State(cfg_svc): State<crate::gateway::clusters::ConfigServices>,
    State(status): State<crate::gateway::clusters::StatusMonitor>,
    req: Request,
) -> axum::response::Response {
    // Guard: require X-Confirm-Restore header
    let headers = req.headers().clone();
    let confirm = headers.get("x-confirm-restore").and_then(|v| v.to_str().ok()).unwrap_or("");
    if confirm != "yes-i-am-sure" {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "restore requires X-Confirm-Restore: yes-i-am-sure header"
        }))).into_response();
    }

    // Size cap
    let cap_mb = cfg_svc.config.limits.max_restore_size_mb;
    let cap_bytes = (cap_mb as usize).saturating_mul(1024 * 1024);
    if let Some((status_code, body)) = check_content_length_cap(&headers, cap_bytes) {
        tracing::warn!(cap_mb, "restore rejected via Content-Length fast-path (payload > cap)");
        return (status_code, [(header::CONTENT_TYPE, "application/json")], body).into_response();
    }
    let body = req.into_body();
    let buf = match drain_body_with_cap(body.into_data_stream(), cap_bytes).await {
        Ok(b) => b,
        Err(CapExceeded { observed_bytes, cap_bytes }) => {
            tracing::warn!(observed_bytes, cap_bytes, "restore rejected via streaming byte-counter");
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": "payload exceeds max_restore_size_mb",
                    "cap_bytes": cap_bytes,
                    "observed_bytes": observed_bytes,
                })),
            ).into_response();
        }
    };

    // Reject non-tar.gz input: JSON format is fully retired (no backward compatibility)
    if buf.len() < 2 || buf[0] != 0x1f || buf[1] != 0x8b {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "unsupported format: only .tar.gz backups (v3) are accepted"
        }))).into_response();
    }

    let container = discover_postgres_container(
        &cfg_svc.config.backup.postgres_container
    ).await;

    // Write .tar.gz to temp file
    let tmpdir = std::env::temp_dir()
        .join(format!("hydeclaw-restore-{}", uuid::Uuid::new_v4()));
    if let Err(e) = tokio::fs::create_dir_all(&tmpdir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("tmpdir: {e}")}))).into_response();
    }
    let tar_path = tmpdir.join("restore.tar.gz");
    if let Err(e) = tokio::fs::write(&tar_path, &buf).await {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("write tar: {e}")}))).into_response();
    }

    // Extract tar
    let extract_dir = tmpdir.join("extracted");
    if let Err(e) = tokio::fs::create_dir_all(&extract_dir).await {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("mkdir extracted: {e}")}))).into_response();
    }
    let tar_out = tokio::process::Command::new("tar")
        .args(["xzf"])
        .arg(&tar_path)
        .arg("-C")
        .arg(&extract_dir)
        .output().await;
    match tar_out {
        Ok(o) if !o.status.success() => {
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": format!("tar extract failed: {}", String::from_utf8_lossy(&o.stderr))
            }))).into_response();
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("tar spawn: {e}")}))).into_response();
        }
        Ok(_) => {}
    }

    // Validate manifest
    let manifest_path = extract_dir.join("manifest.json");
    let manifest: serde_json::Value = match tokio::fs::read(&manifest_path).await {
        Ok(b) => match serde_json::from_slice(&b) {
            Ok(v) => v,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&tmpdir).await;
                return (StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("manifest.json invalid: {e}")}))).into_response();
            }
        },
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("manifest.json missing: {e}")}))).into_response();
        }
    };
    if manifest.get("version").is_none() {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::BAD_REQUEST,
            Json(json!({"error": "unsupported backup format: missing version"}))).into_response();
    }

    // Snapshot workspace for rollback
    let workspace_bak = tmpdir.join("workspace.bak.tar.gz");
    let _ = tokio::process::Command::new("tar")
        .args(["czf"])
        .arg(&workspace_bak)
        .args(["-C", ".", "workspace"])
        .output().await;

    tracing::warn!("RESTORE initiated from pg_dump backup (v3)");

    // Stop all running agents
    {
        let mut map = agents.map.write().await;
        let names: Vec<_> = map.keys().cloned().collect();
        for name in &names {
            if let Some(h) = map.remove(name) {
                h.shutdown(&agents.scheduler).await;
                tracing::info!(agent = %name, "agent stopped for restore");
            }
        }
    }

    // pg_restore
    let dump_path = extract_dir.join("db.dump");
    if let Err(e) = run_pg_restore(&container, &dump_path).await {
        tracing::error!("pg_restore failed: {e}");
        let restarted = restart_agents_from_disk(&agents, &infra, &auth, &bus, &cfg_svc, &status).await;
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
            "error": format!("pg_restore failed: {e}"),
            "agents_restarted_from_old_state": restarted,
        }))).into_response();
    }

    // Restore secrets
    let secrets_bytes = tokio::fs::read(extract_dir.join("secrets.json")).await
        .unwrap_or_default();
    let plaintext_secrets: Vec<crate::secrets::PlaintextSecret> =
        serde_json::from_slice(&secrets_bytes).unwrap_or_default();
    let secret_count = plaintext_secrets.len();
    if let Err(e) = auth.secrets.restore_plaintext(plaintext_secrets).await {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("secrets restore failed: {e}")}))).into_response();
    }

    // Restore workspace and config
    let workspace_src = extract_dir.join("workspace");
    if workspace_src.exists() {
        let workspace_src_str = workspace_src.to_string_lossy().into_owned();
        if let Err(e) = copy_dir_to(&workspace_src_str, std::path::Path::new("workspace")).await {
            // Rollback workspace from snapshot
            let _ = tokio::process::Command::new("tar")
                .args(["xzf"]).arg(&workspace_bak).args(["-C", "."]).output().await;
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("workspace restore failed: {e}")}))).into_response();
        }
    }
    let config_src = extract_dir.join("config");
    if config_src.exists() {
        let config_src_str = config_src.to_string_lossy().into_owned();
        let _ = copy_dir_to(&config_src_str, std::path::Path::new("config")).await;
    }

    // Mark setup complete
    let _ = sqlx::query(
        "INSERT INTO system_flags (key, value) VALUES ('setup_complete', 'true'::jsonb) \
         ON CONFLICT (key) DO UPDATE SET value = 'true'::jsonb, updated_at = NOW()"
    )
    .execute(&infra.db).await
    .inspect_err(|e| tracing::warn!(%e, "restore: set setup_complete failed"));

    // Restart agents from restored configs
    let restarted = restart_agents_from_disk(
        &agents, &infra, &auth, &bus, &cfg_svc, &status
    ).await;

    let _ = tokio::fs::remove_dir_all(&tmpdir).await;
    tracing::warn!(agents = ?restarted, "RESTORE complete (pg_dump v3)");

    Json(json!({
        "ok": true,
        "restored": {
            "db": "pg_restore ok",
            "secrets": secret_count,
        },
        "restarted_agents": restarted,
    })).into_response()
}

/// Core legacy restore logic operating on an already-drained JSON buffer.
/// Called from both `api_restore_legacy` and the new `api_restore` (format auto-detect).
#[allow(clippy::too_many_arguments)]
async fn restore_from_json_buf(
    buf: Vec<u8>,
    infra: InfraServices,
    auth: AuthServices,
    agents: AgentCore,
    bus: crate::gateway::clusters::ChannelBus,
    cfg_svc: crate::gateway::clusters::ConfigServices,
    status: crate::gateway::clusters::StatusMonitor,
) -> axum::response::Response {
    // struson section walk (CONTEXT D-SEC-04). Each section is deserialize_next'd
    // into the typed accumulator; unknown fields are skip_value()-ed for forward-compat.
    let cursor = std::io::Cursor::new(&buf);
    let backup: BackupFile = match parse_backup_stream(cursor) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid backup JSON: {e}")})),
            )
                .into_response();
        }
    };

    tracing::warn!("RESTORE initiated — overwriting configs, secrets, memory, cron");

    // Stop all running agents before restore to prevent stale state
    {
        let mut agents_map = agents.map.write().await;
        let names: Vec<String> = agents_map.keys().cloned().collect();
        for name in &names {
            if let Some(handle) = agents_map.remove(name) {
                handle.shutdown(&agents.scheduler).await;
                tracing::info!(agent = %name, "agent stopped for restore");
            }
        }
    }

    if backup.version != 1 && backup.version != 2 {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "unsupported backup version"}))).into_response();
    }

    let mut restored = json!({ "configs": 0, "workspace_files": 0, "secrets": 0, "memory": 0, "cron": 0 });

    // 1. Config (sync I/O, not in hot path)
    let config_count = restore_config(&backup.config).await;
    restored["configs"] = json!(config_count);

    // 2. Workspace
    let workspace_dir = {
        let deps = agents.deps.read().await;
        deps.workspace_dir.clone()
    };
    let ws_count = restore_workspace(&workspace_dir, &backup.workspace).await;
    restored["workspace_files"] = json!(ws_count);

    // 3. Secrets
    let secret_count = backup.secrets.len();
    match auth.secrets.restore_plaintext(backup.secrets).await {
        Ok(_) => restored["secrets"] = json!(secret_count),
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("secrets restore failed: {e}")}))).into_response();
        }
    }

    // 4+5. Memory + Cron — atomic: both succeed or neither is committed
    let fts_lang = match infra.memory_store.validated_fts_language() {
        Ok(lang) => lang,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("invalid fts language: {e}")}))).into_response();
        }
    };
    match restore_memory_and_cron(&infra.db, &backup.memory, &backup.cron, &fts_lang).await {
        Ok((mem_n, cron_n)) => {
            restored["memory"] = json!(mem_n);
            restored["cron"] = json!(cron_n);
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("db restore failed: {e}")}))).into_response();
        }
    }

    // V2 sections — wrapped in transaction for atomicity (D-10, D-11)
    let mut tx = match infra.db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("v2 restore tx begin failed: {e}")}))).into_response();
        }
    };

    macro_rules! v2_restore {
        ($call:expr, $key:literal) => {
            match $call {
                Ok(n) => { restored[$key] = json!(n); }
                Err(e) => {
                    tracing::error!("V2 restore failed at {}: {}", $key, e);
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("v2 restore failed: {e}")}))).into_response();
                }
            }
        };
    }

    v2_restore!(restore_providers(&mut tx, &backup.providers, &backup.provider_active).await, "providers");
    v2_restore!(restore_channels(&mut tx, &backup.channels).await, "channels");
    v2_restore!(restore_webhooks(&mut tx, &backup.webhooks).await, "webhooks");
    v2_restore!(restore_watchdog_settings(&mut tx, &backup.watchdog_settings).await, "watchdog_settings");
    v2_restore!(restore_allowed_users(&mut tx, &backup.allowed_users).await, "allowed_users");
    v2_restore!(restore_approval_allowlist(&mut tx, &backup.approval_allowlist).await, "approval_allowlist");
    v2_restore!(restore_oauth_accounts(&mut tx, &backup.oauth_accounts).await, "oauth_accounts");
    v2_restore!(restore_oauth_bindings(&mut tx, &backup.oauth_bindings).await, "oauth_bindings");
    v2_restore!(restore_gmail_triggers(&mut tx, &backup.gmail_triggers).await, "gmail_triggers");
    v2_restore!(restore_github_repos(&mut tx, &backup.github_repos).await, "github_repos");

    if let Err(e) = tx.commit().await {
        tracing::error!("V2 restore transaction commit failed: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("v2 restore commit failed: {e}")}))).into_response();
    }

    // Mark setup as complete
    let _ = sqlx::query(
        "INSERT INTO system_flags (key, value) VALUES ('setup_complete', 'true'::jsonb) \
         ON CONFLICT (key) DO UPDATE SET value = 'true'::jsonb, updated_at = NOW()"
    )
    .execute(&infra.db)
    .await
    .inspect_err(|e| tracing::warn!(error = %e, "restore: failed to set setup_complete flag"));

    // Restart agents from restored configs
    let agent_configs = match crate::config::load_agent_configs("config/agents") {
        Ok(cfgs) => cfgs,
        Err(e) => {
            tracing::error!(error = %e, "failed to load agent configs after restore");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("restore succeeded but config reload failed: {}", e)}))).into_response();
        }
    };

    let mut restarted = Vec::new();
    let mut failed = Vec::new();
    for cfg in &agent_configs {
        match super::agents::start_agent_from_config(cfg, &agents, &infra, &auth, &bus, &cfg_svc, &status).await {
            Ok((handle, guard)) => {
                let name = cfg.agent.name.clone();
                agents.map.write().await.insert(name.clone(), handle);
                if let Some(g) = guard {
                    auth.access_guards.write().await.insert(name.clone(), g);
                }
                // Ensure Docker sandbox for non-base agents
                if !cfg.agent.base
                    && let Some(ref sandbox) = infra.sandbox
                    && let Ok(host_path) = std::fs::canonicalize(crate::config::WORKSPACE_DIR)
                    && let Err(e) = sandbox.ensure_container(&name, &host_path.to_string_lossy(), false, Some(&auth.oauth)).await
                {
                    tracing::warn!(agent = %name, error = %e, "failed to ensure container after restore");
                }
                restarted.push(name);
            }
            Err(e) => {
                tracing::error!(agent = %cfg.agent.name, error = %e, "failed to restart agent after restore");
                failed.push(json!({"agent": cfg.agent.name, "error": e.to_string()}));
            }
        }
    }
    tracing::info!(agents = ?restarted, "agents restarted after restore");

    tracing::warn!("AUDIT: system restored from backup: {:?}", restored);
    if failed.is_empty() {
        Json(json!({ "ok": true, "restored": restored, "restarted_agents": restarted, "failed_agents": failed })).into_response()
    } else {
        let failed_names: Vec<&str> = failed.iter()
            .filter_map(|v| v.get("agent").and_then(|a| a.as_str()))
            .collect();
        let warning = format!(
            "restored but {} agent(s) failed to restart: {}",
            failed.len(),
            failed_names.join(", ")
        );
        Json(json!({
            "ok": true,
            "warning": warning,
            "restored": restored,
            "restarted_agents": restarted,
            "failed_agents": failed,
        })).into_response()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn collect_workspace_files(workspace_dir: &str) -> Vec<WorkspaceFile> {
    let mut files = Vec::new();
    let root = FsPath::new(workspace_dir);
    collect_dir(root, root, &mut files).await;
    files
}

fn collect_dir<'a>(
    root: &'a FsPath,
    dir: &'a FsPath,
    files: &'a mut Vec<WorkspaceFile>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let Ok(mut rd) = fs::read_dir(dir).await else { return };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            // Skip noise directories and files
            if matches!(name.as_ref(), "__pycache__" | "node_modules" | ".git" | ".venv") {
                continue;
            }
            if name.ends_with(".pyc") || name.ends_with(".db") {
                continue;
            }
            // Use file_type() from DirEntry — avoids extra stat syscall vs path.is_dir()
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                collect_dir(root, &path, files).await;
            } else {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                if size > 2_097_152 { continue; } // Skip files > 2MB
                if let Ok(bytes) = fs::read(&path).await {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    if let Ok(text) = String::from_utf8(bytes.clone()) {
                        files.push(WorkspaceFile { path: rel_str, content: text });
                    } else {
                        // Binary file (icon, image): base64 encode
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        files.push(WorkspaceFile { path: rel_str, content: format!("base64:{b64}") });
                    }
                }
            }
        }
    })
}

const MEMORY_BACKUP_LIMIT: i64 = 100_000;

/// DB-only variant of `collect_memory` — used by `create_backup_internal` (no `AppState`).
async fn collect_memory_from_db(db: &PgPool) -> sqlx::Result<Vec<MemoryChunk>> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks")
        .fetch_one(db).await.unwrap_or(0);
    if total > MEMORY_BACKUP_LIMIT {
        tracing::warn!(total, limit = MEMORY_BACKUP_LIMIT, "memory_chunks exceeds backup limit, truncating");
    }

    #[allow(clippy::type_complexity)]
    let rows: Vec<(uuid::Uuid, String, String, Option<String>, bool, f64, chrono::DateTime<Utc>, Option<uuid::Uuid>, i32, String, Option<String>, Option<String>, bool)> =
        sqlx::query_as(
            "SELECT id, agent_id, content, source, pinned, relevance_score, created_at, parent_id, chunk_index,
                    COALESCE(scope, 'private'), category, topic, COALESCE(archived, false)
             FROM memory_chunks ORDER BY created_at LIMIT $1",
        )
        .bind(MEMORY_BACKUP_LIMIT)
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(id, agent_id, content, source, pinned, relevance_score, created_at, parent_id, chunk_index, scope, category, topic, archived)| MemoryChunk {
            id: id.to_string(),
            agent_id,
            user_id: None,
            content,
            source,
            pinned,
            relevance_score,
            created_at,
            parent_id: parent_id.map(|p| p.to_string()),
            chunk_index,
            scope,
            category,
            topic,
            archived,
        })
        .collect())
}

/// DB-only variant of `collect_cron` — used by `create_backup_internal` (no `AppState`).
async fn collect_cron_from_db(db: &PgPool) -> sqlx::Result<Vec<CronJob>> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, String, String, String, bool, Option<Value>, bool)> =
        sqlx::query_as(
            "SELECT agent_id, name, cron_expr, timezone, task_message, enabled, announce_to, silent
             FROM scheduled_jobs ORDER BY name",
        )
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(agent_id, name, cron_expr, timezone, task_message, enabled, announce_to, silent)| CronJob {
            agent_id,
            name,
            cron_expr,
            timezone,
            task_message,
            enabled,
            announce_to,
            silent,
        })
        .collect())
}

// ── V2 collectors ─────────────────────────────────────────────────────────────

async fn collect_providers(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupProvider>> {
    let rows = crate::db::providers::list_providers(db).await?;
    Ok(rows.iter().map(|r| BackupProvider {
        id: r.id.to_string(),
        name: r.name.clone(),
        category: r.category.clone(),
        provider_type: r.provider_type.clone(),
        base_url: r.base_url.clone(),
        default_model: r.default_model.clone(),
        enabled: r.enabled,
        options: r.options.clone(),
        notes: r.notes.clone(),
    }).collect())
}

async fn collect_provider_active(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupProviderActive>> {
    let rows = crate::db::providers::list_provider_active(db).await?;
    Ok(rows.iter().filter_map(|r| {
        r.provider_name.as_ref().map(|pn| BackupProviderActive {
            capability: r.capability.clone(),
            provider_name: pn.clone(),
        })
    }).collect())
}

async fn collect_channels(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupChannel>> {
    let rows = sqlx::query(
        "SELECT id, agent_name, channel_type, display_name, config, status FROM agent_channels WHERE status != 'deleted'"
    ).fetch_all(db).await?;
    let credential_keys = ["bot_token", "access_token", "password", "app_token"];
    Ok(rows.iter().map(|r| {
        let mut config: Value = r.get("config");
        // Redact any legacy credentials that might still be in the config JSONB
        if let Some(cfg) = config.as_object_mut() {
            for key in &credential_keys {
                cfg.remove(*key);
            }
        }
        BackupChannel {
            id: r.get::<uuid::Uuid, _>("id").to_string(),
            agent_name: r.get("agent_name"),
            channel_type: r.get("channel_type"),
            display_name: r.get("display_name"),
            config,
            status: r.get("status"),
        }
    }).collect())
}

async fn collect_webhooks(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupWebhook>> {
    let rows = sqlx::query(
        "SELECT name, agent_id, secret, prompt_prefix, enabled, webhook_type, event_filter FROM webhooks"
    ).fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupWebhook {
        name: r.get("name"),
        agent_id: r.get("agent_id"),
        secret: r.get("secret"),
        prompt_prefix: r.get("prompt_prefix"),
        enabled: r.get("enabled"),
        webhook_type: r.get("webhook_type"),
        event_filter: r.get("event_filter"),
    }).collect())
}

async fn collect_watchdog_settings(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupWatchdogSetting>> {
    let rows = sqlx::query("SELECT key, value FROM watchdog_settings")
        .fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupWatchdogSetting {
        key: r.get("key"),
        value: r.get("value"),
    }).collect())
}

async fn collect_allowed_users(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupAllowedUser>> {
    let rows = sqlx::query("SELECT agent_id, channel_user_id, display_name FROM channel_allowed_users")
        .fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupAllowedUser {
        agent_id: r.get("agent_id"),
        channel_user_id: r.get("channel_user_id"),
        display_name: r.get("display_name"),
    }).collect())
}

async fn collect_approval_allowlist(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupApprovalAllow>> {
    let rows = sqlx::query("SELECT agent_id, tool_pattern FROM approval_allowlist")
        .fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupApprovalAllow {
        agent_id: r.get("agent_id"),
        tool_pattern: r.get("tool_pattern"),
    }).collect())
}

async fn collect_oauth_accounts(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupOAuthAccount>> {
    let rows = sqlx::query("SELECT id, provider, display_name, scope, status FROM oauth_accounts")
        .fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupOAuthAccount {
        id: r.get::<uuid::Uuid, _>("id").to_string(),
        provider: r.get("provider"),
        display_name: r.get("display_name"),
        scope: r.get("scope"),
        status: r.get("status"),
    }).collect())
}

async fn collect_oauth_bindings(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupOAuthBinding>> {
    let rows = sqlx::query("SELECT agent_id, provider, account_id FROM agent_oauth_bindings")
        .fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupOAuthBinding {
        agent_id: r.get("agent_id"),
        provider: r.get("provider"),
        account_id: r.get::<uuid::Uuid, _>("account_id").to_string(),
    }).collect())
}

async fn collect_gmail_triggers(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupGmailTrigger>> {
    let rows = sqlx::query("SELECT agent_id, email_address, pubsub_topic, enabled FROM gmail_triggers")
        .fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupGmailTrigger {
        agent_id: r.get("agent_id"),
        email_address: r.get("email_address"),
        pubsub_topic: r.get("pubsub_topic"),
        enabled: r.get("enabled"),
    }).collect())
}

async fn collect_github_repos(db: &sqlx::PgPool) -> sqlx::Result<Vec<BackupGithubRepo>> {
    let rows = sqlx::query("SELECT agent_id, owner, repo FROM agent_github_repos")
        .fetch_all(db).await?;
    Ok(rows.iter().map(|r| BackupGithubRepo {
        agent_id: r.get("agent_id"),
        owner: r.get("owner"),
        repo: r.get("repo"),
    }).collect())
}

// ── V2 restore helpers ────────────────────────────────────────────────────────

async fn restore_providers(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, providers: &[BackupProvider], active: &[BackupProviderActive]) -> sqlx::Result<usize> {
    if providers.is_empty() && active.is_empty() {
        return Ok(0);
    }
    sqlx::query("DELETE FROM provider_active").execute(&mut **tx).await?;
    sqlx::query("DELETE FROM providers").execute(&mut **tx).await?;

    let mut count = 0;
    for p in providers {
        let id: uuid::Uuid = p.id.parse().unwrap_or_else(|_| uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO providers (id, name, type, provider_type, base_url, default_model, enabled, options, notes) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
        )
        .bind(id).bind(&p.name).bind(&p.category).bind(&p.provider_type)
        .bind(&p.base_url).bind(&p.default_model).bind(p.enabled)
        .bind(&p.options).bind(&p.notes)
        .execute(&mut **tx).await?;
        count += 1;
    }

    for a in active {
        sqlx::query(
            "INSERT INTO provider_active (capability, provider_name) VALUES ($1, $2) ON CONFLICT DO NOTHING"
        )
        .bind(&a.capability).bind(&a.provider_name)
        .execute(&mut **tx).await?;
    }

    Ok(count)
}

async fn restore_channels(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupChannel]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM agent_channels").execute(&mut **tx).await?;
    for c in items {
        let id = uuid::Uuid::parse_str(&c.id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO agent_channels (id, agent_name, channel_type, display_name, config, status)
             VALUES ($1, $2, $3, $4, $5, $6)"
        )
        .bind(id).bind(&c.agent_name).bind(&c.channel_type).bind(&c.display_name)
        .bind(&c.config).bind(&c.status)
        .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_webhooks(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupWebhook]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM webhooks").execute(&mut **tx).await?;
    for w in items {
        sqlx::query(
            "INSERT INTO webhooks (name, agent_id, secret, prompt_prefix, enabled, webhook_type, event_filter)
             VALUES ($1, $2, $3, $4, $5, $6, $7)"
        )
        .bind(&w.name).bind(&w.agent_id).bind(&w.secret).bind(&w.prompt_prefix)
        .bind(w.enabled).bind(&w.webhook_type).bind(&w.event_filter as &Option<Vec<String>>)
        .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_watchdog_settings(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupWatchdogSetting]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM watchdog_settings").execute(&mut **tx).await?;
    for s in items {
        sqlx::query("INSERT INTO watchdog_settings (key, value) VALUES ($1, $2)")
            .bind(&s.key).bind(&s.value)
            .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_approval_allowlist(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupApprovalAllow]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM approval_allowlist").execute(&mut **tx).await?;
    for e in items {
        sqlx::query("INSERT INTO approval_allowlist (agent_id, tool_pattern) VALUES ($1, $2)")
            .bind(&e.agent_id).bind(&e.tool_pattern)
            .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_allowed_users(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupAllowedUser]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM channel_allowed_users").execute(&mut **tx).await?;
    for u in items {
        sqlx::query(
            "INSERT INTO channel_allowed_users (agent_id, channel_user_id, display_name) VALUES ($1, $2, $3)"
        )
        .bind(&u.agent_id).bind(&u.channel_user_id).bind(&u.display_name)
        .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_oauth_accounts(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupOAuthAccount]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM agent_oauth_bindings").execute(&mut **tx).await?;
    sqlx::query("DELETE FROM oauth_accounts").execute(&mut **tx).await?;
    for a in items {
        let id = uuid::Uuid::parse_str(&a.id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        sqlx::query("INSERT INTO oauth_accounts (id, provider, display_name, scope, status) VALUES ($1, $2, $3, $4, $5)")
            .bind(id).bind(&a.provider).bind(&a.display_name).bind(&a.scope).bind(&a.status)
            .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_oauth_bindings(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupOAuthBinding]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    for b in items {
        let account_id = uuid::Uuid::parse_str(&b.account_id).unwrap_or_default();
        sqlx::query("INSERT INTO agent_oauth_bindings (agent_id, provider, account_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(&b.agent_id).bind(&b.provider).bind(account_id)
            .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_gmail_triggers(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupGmailTrigger]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM gmail_triggers").execute(&mut **tx).await?;
    for t in items {
        sqlx::query("INSERT INTO gmail_triggers (agent_id, email_address, pubsub_topic, enabled) VALUES ($1, $2, $3, $4)")
            .bind(&t.agent_id).bind(&t.email_address).bind(&t.pubsub_topic).bind(t.enabled)
            .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn restore_github_repos(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, items: &[BackupGithubRepo]) -> sqlx::Result<usize> {
    if items.is_empty() { return Ok(0); }
    sqlx::query("DELETE FROM agent_github_repos").execute(&mut **tx).await?;
    for r in items {
        sqlx::query("INSERT INTO agent_github_repos (agent_id, owner, repo) VALUES ($1, $2, $3)")
            .bind(&r.agent_id).bind(&r.owner).bind(&r.repo)
            .execute(&mut **tx).await?;
    }
    Ok(items.len())
}

async fn cleanup_old_backups_with_retention(now: chrono::DateTime<Utc>, retention_days: i64) {
    let cutoff = now - chrono::Duration::days(retention_days);
    let Ok(mut dir) = fs::read_dir(BACKUP_DIR).await else { return };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            // Parse date from filename: hydeclaw-YYYY-MM-DD.json
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && let Some(date_part) = stem.strip_prefix("hydeclaw-")
                    && let Ok(date) = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d") {
                        let file_dt = date.and_hms_opt(0, 0, 0).expect("midnight is valid time").and_utc();
                        if file_dt < cutoff {
                            let _ = fs::remove_file(&path).await;
                            tracing::info!(path = %path.display(), "removed old backup");
                        }
                    }
        }
    }
}

async fn restore_config(config: &Value) -> usize {
    let mut count = 0;
    if let Some(toml_str) = config.get("app_config").and_then(|v| v.as_str())
        && toml_str.parse::<toml::Table>().is_ok() {
            let _ = fs::copy("config/hydeclaw.toml", "config/hydeclaw.toml.bak").await;
            if fs::write("config/hydeclaw.toml", toml_str).await.is_ok() {
                count += 1;
            }
        }
    if let Some(agents) = config.get("agents").and_then(|v| v.as_object()) {
        let _ = fs::create_dir_all("config/agents").await;
        for (name, content) in agents {
            // Validate agent name (same rules as API create)
            if name.contains('/') || name.contains('\\') || name.contains("..")
               || name.is_empty() || name.len() > 64
               || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ') {
                continue;
            }
            if let Some(toml_str) = content.as_str()
                && toml_str.parse::<toml::Table>().is_ok() {
                    let path = format!("config/agents/{name}.toml");
                    let _ = fs::copy(&path, format!("{path}.bak")).await;
                    if fs::write(&path, toml_str).await.is_ok() { count += 1; }
                }
        }
    }
    count
}

async fn restore_workspace(workspace_dir: &str, files: &[WorkspaceFile]) -> usize {
    let root = FsPath::new(workspace_dir);
    let mut count = 0;
    for file in files {
        // Prevent path traversal
        if file.path.contains("..") || file.path.starts_with('/') || file.path.starts_with('\\') { continue; }
        let dest = root.join(&file.path);
        let bytes = if file.content.starts_with("base64:") {
            use base64::Engine;
            match base64::engine::general_purpose::STANDARD.decode(&file.content[7..]) {
                Ok(b) => b,
                Err(_) => file.content.as_bytes().to_vec(),
            }
        } else {
            file.content.as_bytes().to_vec()
        };
        if let Some(parent) = dest.parent()
            && fs::create_dir_all(parent).await.is_ok()
                && fs::write(&dest, &bytes).await.is_ok() {
                    count += 1;
                }
    }
    count
}

/// Restore memory and cron jobs atomically within a single DB transaction.
/// Preserves the `daily-backup` cron job so the base agent continues working after restore.
async fn restore_memory_and_cron(
    db: &PgPool,
    chunks: &[MemoryChunk],
    jobs: &[CronJob],
    fts_lang: &str,
) -> sqlx::Result<(usize, usize)> {
    let mut tx = db.begin().await?;

    // Disable FK checks for bulk restore (parent_id references may arrive out of order)
    sqlx::query("SET CONSTRAINTS ALL DEFERRED").execute(&mut *tx).await?;

    // Memory: replace all chunks
    sqlx::query("DELETE FROM memory_chunks").execute(&mut *tx).await?;
    for chunk in chunks {
        let id = uuid::Uuid::parse_str(&chunk.id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        let parent_id = chunk.parent_id.as_deref().and_then(|s| uuid::Uuid::parse_str(s).ok());
        let scope = if chunk.scope.is_empty() { "private" } else { &chunk.scope };
        sqlx::query(
            "INSERT INTO memory_chunks (id, agent_id, content, source, pinned, relevance_score, created_at, tsv, parent_id, chunk_index, scope, category, topic, archived)
             VALUES ($1, $2, $3, $4, $5, $6, $7, to_tsvector($8::regconfig, $3), $9, $10, $11, $12, $13, $14)",
        )
        .bind(id)
        .bind(&chunk.agent_id)
        .bind(&chunk.content)
        .bind(&chunk.source)
        .bind(chunk.pinned)
        .bind(chunk.relevance_score)
        .bind(chunk.created_at)
        .bind(fts_lang)
        .bind(parent_id)
        .bind(chunk.chunk_index)
        .bind(scope)
        .bind(&chunk.category)
        .bind(&chunk.topic)
        .bind(chunk.archived)
        .execute(&mut *tx)
        .await?;
    }

    // Cron: replace all jobs except daily-backup (base agent re-creates it on heartbeat anyway,
    // but preserving it means backups keep running even if base agent hasn't heartbeated yet)
    sqlx::query("DELETE FROM scheduled_jobs WHERE name != 'daily-backup'")
        .execute(&mut *tx)
        .await?;
    for job in jobs {
        if job.name == "daily-backup" { continue; } // already preserved above
        sqlx::query(
            "INSERT INTO scheduled_jobs (agent_id, name, cron_expr, timezone, task_message, enabled, announce_to, silent)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (name) DO UPDATE SET
               cron_expr = EXCLUDED.cron_expr,
               timezone = EXCLUDED.timezone,
               task_message = EXCLUDED.task_message,
               enabled = EXCLUDED.enabled,
               announce_to = EXCLUDED.announce_to,
               silent = EXCLUDED.silent",
        )
        .bind(&job.agent_id)
        .bind(&job.name)
        .bind(&job.cron_expr)
        .bind(&job.timezone)
        .bind(&job.task_message)
        .bind(job.enabled)
        .bind(&job.announce_to)
        .bind(job.silent)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok((chunks.len(), jobs.len()))
}

// ── Phase 64 SEC-04 — struson section-walker for BackupFile ─────────────────

/// Fatal error while streaming-parsing the restore payload.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BackupParseError {
    #[error("{0}")]
    Parse(String),
}

/// struson-backed streaming BackupFile parser. Walks the top-level JSON object
/// section-by-section; each known field is deserialized via `JsonReader::deserialize_next`
/// (struson 0.6 `serde` feature). Unknown fields are `skip_value()`-ed for forward
/// compatibility.
///
/// Peak heap bound: the buffer backing the reader PLUS the single largest section
/// typed out at once. For a 100 MiB BackupFile that's roughly 100 MiB (reader buffer)
/// + ~50 MiB (largest workspace section) — well under the 150 MiB CONTEXT cap.
///
/// NO `serde_json::from_slice(&buf)` fallback exists: that would materialise the
/// whole `Value` graph and violate CONTEXT D-SEC-04.
pub(crate) fn parse_backup_stream<R: std::io::Read>(
    reader: R,
) -> std::result::Result<BackupFile, BackupParseError> {
    let mut json = JsonStreamReader::new(reader);

    // Accumulators — one per top-level BackupFile field. `Option` lets us detect
    // required-but-missing fields and preserve `#[serde(default)]` semantics for
    // the v2 extras.
    let mut version: Option<u32> = None;
    let mut created_at: Option<chrono::DateTime<Utc>> = None;
    let mut config: Option<Value> = None;
    let mut workspace: Option<Vec<WorkspaceFile>> = None;
    let mut secrets_: Option<Vec<PlaintextSecret>> = None;
    let mut memory: Option<Vec<MemoryChunk>> = None;
    let mut cron: Option<Vec<CronJob>> = None;
    let mut providers: Option<Vec<BackupProvider>> = None;
    let mut provider_active: Option<Vec<BackupProviderActive>> = None;
    let mut channels: Option<Vec<BackupChannel>> = None;
    let mut webhooks: Option<Vec<BackupWebhook>> = None;
    let mut watchdog_settings: Option<Vec<BackupWatchdogSetting>> = None;
    let mut allowed_users: Option<Vec<BackupAllowedUser>> = None;
    let mut approval_allowlist: Option<Vec<BackupApprovalAllow>> = None;
    let mut oauth_accounts: Option<Vec<BackupOAuthAccount>> = None;
    let mut oauth_bindings: Option<Vec<BackupOAuthBinding>> = None;
    let mut gmail_triggers: Option<Vec<BackupGmailTrigger>> = None;
    let mut github_repos: Option<Vec<BackupGithubRepo>> = None;

    json.begin_object()
        .map_err(|e| BackupParseError::Parse(format!("begin_object: {e}")))?;

    while json
        .has_next()
        .map_err(|e| BackupParseError::Parse(format!("has_next: {e}")))?
    {
        let name = json
            .next_name_owned()
            .map_err(|e| BackupParseError::Parse(format!("next_name: {e}")))?;

        match name.as_str() {
            "version" => {
                version = Some(
                    json.deserialize_next::<u32>()
                        .map_err(|e| BackupParseError::Parse(format!("version: {e}")))?,
                );
            }
            "created_at" => {
                created_at = Some(
                    json.deserialize_next::<chrono::DateTime<Utc>>()
                        .map_err(|e| BackupParseError::Parse(format!("created_at: {e}")))?,
                );
            }
            "config" => {
                config = Some(
                    json.deserialize_next::<Value>()
                        .map_err(|e| BackupParseError::Parse(format!("config: {e}")))?,
                );
            }
            "workspace" => {
                workspace = Some(
                    json.deserialize_next::<Vec<WorkspaceFile>>()
                        .map_err(|e| BackupParseError::Parse(format!("workspace: {e}")))?,
                );
            }
            "secrets" => {
                secrets_ = Some(
                    json.deserialize_next::<Vec<PlaintextSecret>>()
                        .map_err(|e| BackupParseError::Parse(format!("secrets: {e}")))?,
                );
            }
            "memory" => {
                memory = Some(
                    json.deserialize_next::<Vec<MemoryChunk>>()
                        .map_err(|e| BackupParseError::Parse(format!("memory: {e}")))?,
                );
            }
            "cron" => {
                cron = Some(
                    json.deserialize_next::<Vec<CronJob>>()
                        .map_err(|e| BackupParseError::Parse(format!("cron: {e}")))?,
                );
            }
            "providers" => {
                providers = Some(
                    json.deserialize_next::<Vec<BackupProvider>>()
                        .map_err(|e| BackupParseError::Parse(format!("providers: {e}")))?,
                );
            }
            "provider_active" => {
                provider_active = Some(
                    json.deserialize_next::<Vec<BackupProviderActive>>()
                        .map_err(|e| BackupParseError::Parse(format!("provider_active: {e}")))?,
                );
            }
            "channels" => {
                channels = Some(
                    json.deserialize_next::<Vec<BackupChannel>>()
                        .map_err(|e| BackupParseError::Parse(format!("channels: {e}")))?,
                );
            }
            "webhooks" => {
                webhooks = Some(
                    json.deserialize_next::<Vec<BackupWebhook>>()
                        .map_err(|e| BackupParseError::Parse(format!("webhooks: {e}")))?,
                );
            }
            "watchdog_settings" => {
                watchdog_settings = Some(
                    json.deserialize_next::<Vec<BackupWatchdogSetting>>()
                        .map_err(|e| BackupParseError::Parse(format!("watchdog_settings: {e}")))?,
                );
            }
            "allowed_users" => {
                allowed_users = Some(
                    json.deserialize_next::<Vec<BackupAllowedUser>>()
                        .map_err(|e| BackupParseError::Parse(format!("allowed_users: {e}")))?,
                );
            }
            "approval_allowlist" => {
                approval_allowlist = Some(
                    json.deserialize_next::<Vec<BackupApprovalAllow>>()
                        .map_err(|e| BackupParseError::Parse(format!("approval_allowlist: {e}")))?,
                );
            }
            "oauth_accounts" => {
                oauth_accounts = Some(
                    json.deserialize_next::<Vec<BackupOAuthAccount>>()
                        .map_err(|e| BackupParseError::Parse(format!("oauth_accounts: {e}")))?,
                );
            }
            "oauth_bindings" => {
                oauth_bindings = Some(
                    json.deserialize_next::<Vec<BackupOAuthBinding>>()
                        .map_err(|e| BackupParseError::Parse(format!("oauth_bindings: {e}")))?,
                );
            }
            "gmail_triggers" => {
                gmail_triggers = Some(
                    json.deserialize_next::<Vec<BackupGmailTrigger>>()
                        .map_err(|e| BackupParseError::Parse(format!("gmail_triggers: {e}")))?,
                );
            }
            "github_repos" => {
                github_repos = Some(
                    json.deserialize_next::<Vec<BackupGithubRepo>>()
                        .map_err(|e| BackupParseError::Parse(format!("github_repos: {e}")))?,
                );
            }
            _ => {
                // Forward-compat: unknown top-level fields are skipped, not an error.
                json.skip_value()
                    .map_err(|e| BackupParseError::Parse(format!("skip_value({name}): {e}")))?;
            }
        }
    }

    json.end_object()
        .map_err(|e| BackupParseError::Parse(format!("end_object: {e}")))?;

    Ok(BackupFile {
        version: version.ok_or_else(|| BackupParseError::Parse("missing version".into()))?,
        created_at: created_at
            .ok_or_else(|| BackupParseError::Parse("missing created_at".into()))?,
        config: config.unwrap_or_else(|| Value::Object(serde_json::Map::new())),
        workspace: workspace.unwrap_or_default(),
        secrets: secrets_.unwrap_or_default(),
        memory: memory.unwrap_or_default(),
        cron: cron.unwrap_or_default(),
        providers: providers.unwrap_or_default(),
        provider_active: provider_active.unwrap_or_default(),
        channels: channels.unwrap_or_default(),
        webhooks: webhooks.unwrap_or_default(),
        watchdog_settings: watchdog_settings.unwrap_or_default(),
        allowed_users: allowed_users.unwrap_or_default(),
        approval_allowlist: approval_allowlist.unwrap_or_default(),
        oauth_accounts: oauth_accounts.unwrap_or_default(),
        oauth_bindings: oauth_bindings.unwrap_or_default(),
        gmail_triggers: gmail_triggers.unwrap_or_default(),
        github_repos: github_repos.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Per D-12: Round-trip test proves backup export -> restore preserves all provider data
    /// without loss or corruption. This tests serialization fidelity; actual DB round-trip
    /// requires integration test infrastructure.
    #[test]
    fn test_backup_roundtrip_providers() {
        // Construct realistic providers covering all types
        let providers = vec![
            BackupProvider {
                id: "550e8400-e29b-41d4-a716-446655440001".to_string(),
                name: "openai-main".to_string(),
                category: "text".to_string(),
                provider_type: "openai".to_string(),
                base_url: Some("https://api.openai.com/v1".to_string()),
                default_model: Some("gpt-4o".to_string()),
                enabled: true,
                options: json!({"models": ["gpt-4o", "gpt-4o-mini"], "max_tokens": 8192}),
                notes: Some("Primary text provider".to_string()),
            },
            BackupProvider {
                id: "550e8400-e29b-41d4-a716-446655440002".to_string(),
                name: "ollama-embed".to_string(),
                category: "embedding".to_string(),
                provider_type: "ollama".to_string(),
                base_url: Some("http://localhost:11434".to_string()),
                default_model: Some("nomic-embed-text".to_string()),
                enabled: true,
                options: json!({}),
                notes: None,
            },
            BackupProvider {
                id: "550e8400-e29b-41d4-a716-446655440003".to_string(),
                name: "whisper-stt".to_string(),
                category: "stt".to_string(),
                provider_type: "whisper".to_string(),
                base_url: None,
                default_model: None,
                enabled: false,
                options: json!({"models": []}),
                notes: Some("".to_string()),
            },
            // Edge case: unicode in provider name
            BackupProvider {
                id: "550e8400-e29b-41d4-a716-446655440004".to_string(),
                name: "tts-\u{00e9}l\u{00e8}ve".to_string(),
                category: "tts".to_string(),
                provider_type: "custom".to_string(),
                base_url: Some("http://192.168.1.132:8880".to_string()),
                default_model: Some("clone:Agent1".to_string()),
                enabled: true,
                options: json!(null),
                notes: None,
            },
        ];

        let active = vec![
            BackupProviderActive {
                capability: "text".to_string(),
                provider_name: "openai-main".to_string(),
            },
            BackupProviderActive {
                capability: "embedding".to_string(),
                provider_name: "ollama-embed".to_string(),
            },
        ];

        // Serialize (simulating export)
        let providers_json = serde_json::to_string(&providers).expect("serialize providers");
        let active_json = serde_json::to_string(&active).expect("serialize active");

        // Deserialize (simulating restore parse)
        let restored_providers: Vec<BackupProvider> =
            serde_json::from_str(&providers_json).expect("deserialize providers");
        let restored_active: Vec<BackupProviderActive> =
            serde_json::from_str(&active_json).expect("deserialize active");

        // Assert full equality
        assert_eq!(providers, restored_providers, "providers round-trip mismatch");
        assert_eq!(active, restored_active, "provider_active round-trip mismatch");

        // Verify specific edge cases survived the round-trip
        let whisper = &restored_providers[2];
        assert_eq!(whisper.base_url, None, "None base_url should survive round-trip");
        assert_eq!(whisper.default_model, None, "None default_model should survive round-trip");
        assert_eq!(whisper.options, json!({"models": []}), "empty models list should survive round-trip");
        assert!(!whisper.enabled, "disabled flag should survive round-trip");

        let unicode_provider = &restored_providers[3];
        assert_eq!(unicode_provider.name, "tts-\u{00e9}l\u{00e8}ve", "unicode name should survive round-trip");
        assert_eq!(unicode_provider.notes, None, "None notes should survive round-trip");
        assert_eq!(unicode_provider.options, json!(null), "null options should survive round-trip");

        // Verify round-trip through BackupFile container (full envelope)
        let wrapper = json!({
            "providers": providers,
            "provider_active": active,
        });
        let wrapper_json = serde_json::to_string(&wrapper).expect("serialize wrapper");
        let restored_wrapper: serde_json::Value =
            serde_json::from_str(&wrapper_json).expect("deserialize wrapper");

        let final_providers: Vec<BackupProvider> =
            serde_json::from_value(restored_wrapper["providers"].clone()).expect("extract providers");
        let final_active: Vec<BackupProviderActive> =
            serde_json::from_value(restored_wrapper["provider_active"].clone()).expect("extract active");

        assert_eq!(providers, final_providers, "nested round-trip providers mismatch");
        assert_eq!(active, final_active, "nested round-trip active mismatch");
    }

    // ── Phase 64 SEC-04: struson section-walker unit tests ─────────────────

    #[test]
    fn parse_backup_stream_happy_path() {
        let payload = br#"{
            "version": 2,
            "created_at": "2026-01-01T00:00:00Z",
            "config": {},
            "workspace": [{"path":"a.txt","content":"hello"}],
            "secrets": [],
            "memory": [],
            "cron": []
        }"#;
        let parsed = parse_backup_stream(std::io::Cursor::new(&payload[..])).unwrap();
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.workspace.len(), 1);
        assert_eq!(parsed.workspace[0].path, "a.txt");
        assert_eq!(parsed.workspace[0].content, "hello");
        assert!(parsed.providers.is_empty());
        assert!(parsed.channels.is_empty());
    }

    #[test]
    fn parse_backup_stream_missing_version_fails() {
        let payload = br#"{
            "created_at": "2026-01-01T00:00:00Z",
            "config": {},
            "workspace": [],
            "secrets": [],
            "memory": [],
            "cron": []
        }"#;
        let err = parse_backup_stream(std::io::Cursor::new(&payload[..])).unwrap_err();
        assert!(matches!(err, BackupParseError::Parse(_)));
    }

    #[test]
    fn parse_backup_stream_missing_created_at_fails() {
        let payload = br#"{
            "version": 2,
            "config": {},
            "workspace": [],
            "secrets": [],
            "memory": [],
            "cron": []
        }"#;
        let err = parse_backup_stream(std::io::Cursor::new(&payload[..])).unwrap_err();
        assert!(matches!(err, BackupParseError::Parse(_)));
    }

    #[test]
    fn parse_backup_stream_unknown_fields_skipped() {
        let payload = br#"{
            "version": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "config": {},
            "workspace": [],
            "secrets": [],
            "memory": [],
            "cron": [],
            "future_field_2099": {"we": "ignore this", "nested": [1,2,3]},
            "another_unknown": "string value"
        }"#;
        let parsed = parse_backup_stream(std::io::Cursor::new(&payload[..])).unwrap();
        assert_eq!(parsed.version, 1);
    }

    /// Parity with `serde_json::from_slice` on a realistic small backup. Proves the
    /// struson walker produces an equivalent `BackupFile` for every field. Required
    /// by plan Task 2b acceptance — guards against silent field-skipping regressions.
    #[test]
    fn parse_backup_stream_parity_with_serde_json() {
        let payload = br##"{
            "version": 2,
            "created_at": "2026-01-01T00:00:00Z",
            "config": {"app_config": "x"},
            "workspace": [
                {"path":"a.txt","content":"hello"},
                {"path":"b/c.md","content":"# title"}
            ],
            "secrets": [],
            "memory": [],
            "cron": [],
            "providers": [{
                "id": "550e8400-e29b-41d4-a716-446655440001",
                "name": "openai",
                "type": "text",
                "provider_type": "openai",
                "base_url": null,
                "default_model": "gpt-4o",
                "enabled": true,
                "options": {},
                "notes": null
            }],
            "provider_active": [{"capability":"text","provider_name":"openai"}],
            "channels": [],
            "webhooks": [],
            "watchdog_settings": [],
            "allowed_users": [],
            "approval_allowlist": [],
            "oauth_accounts": [],
            "oauth_bindings": [],
            "gmail_triggers": [],
            "github_repos": []
        }"##;
        let via_serde: BackupFile = serde_json::from_slice(&payload[..]).unwrap();
        let via_struson = parse_backup_stream(std::io::Cursor::new(&payload[..])).unwrap();

        assert_eq!(via_struson.version, via_serde.version);
        assert_eq!(via_struson.created_at, via_serde.created_at);
        assert_eq!(via_struson.workspace.len(), via_serde.workspace.len());
        assert_eq!(via_struson.workspace[0].path, via_serde.workspace[0].path);
        assert_eq!(via_struson.workspace[1].content, via_serde.workspace[1].content);
        assert_eq!(via_struson.providers.len(), via_serde.providers.len());
        assert_eq!(via_struson.providers[0], via_serde.providers[0]);
        assert_eq!(via_struson.provider_active, via_serde.provider_active);
    }

    #[test]
    fn parse_container_name_returns_first_non_empty_line() {
        assert_eq!(
            parse_container_name("docker-postgres-1\ndocker-postgres-2\n", "fallback"),
            "docker-postgres-1"
        );
    }

    #[test]
    fn parse_container_name_falls_back_when_output_empty() {
        assert_eq!(parse_container_name("", "docker-postgres-1"), "docker-postgres-1");
    }

    #[test]
    fn parse_container_name_trims_whitespace() {
        assert_eq!(parse_container_name("  my-pg-1  \n", "fb"), "my-pg-1");
    }

    #[test]
    fn excluded_tables_contains_secrets_and_sessions() {
        assert!(EXCLUDED_TABLES.contains(&"secrets"));
        assert!(EXCLUDED_TABLES.contains(&"sessions"));
        assert!(EXCLUDED_TABLES.contains(&"messages"));
        assert!(EXCLUDED_TABLES.contains(&"pending_messages"));
        assert!(EXCLUDED_TABLES.contains(&"outbound_queue"));
    }

    #[test]
    fn pg_dump_excludes_all_required_tables() {
        // Build the args vector the same way run_pg_dump does, verify all tables present.
        let mut args: Vec<String> = vec![
            "exec".into(), "pg".into(),
            "pg_dump".into(), "-U".into(), "hydeclaw".into(),
            "hydeclaw".into(), "-Fc".into(),
        ];
        for t in EXCLUDED_TABLES {
            args.push("--exclude-table".into());
            args.push(t.to_string());
        }
        // secrets must be excluded
        let pairs: Vec<_> = args.windows(2).collect();
        assert!(pairs.iter().any(|w| w[0] == "--exclude-table" && w[1] == "secrets"));
        assert!(pairs.iter().any(|w| w[0] == "--exclude-table" && w[1] == "messages"));
        assert!(pairs.iter().any(|w| w[0] == "--exclude-table" && w[1] == "outbound_queue"));
        // total --exclude-table flags == EXCLUDED_TABLES length
        let count = args.iter().filter(|a| *a == "--exclude-table").count();
        assert_eq!(count, EXCLUDED_TABLES.len());
    }
}
