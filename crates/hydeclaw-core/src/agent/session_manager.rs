//! Session lifecycle management for agent engines.
//!
//! This module centralises all session create/resume/load/save/status operations
//! so they can be delegated from engine.rs through a single `SessionManager` handle.
//! Pure utility functions (`resolve_dm_scope`, `truncate_title`) are unit-testable
//! without a database connection.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::sessions::MessageRow;
use hydeclaw_db::sessions::warn_invalid_transition;
use hydeclaw_db::SessionStatus;

// ── SessionManager ──────────────────────────────────────────────────────────

/// Thin wrapper around `crate::db::sessions::*` that groups all session
/// lifecycle operations in one place.
///
/// `PgPool` is clone-cheap (`Arc` internally), so constructing a `SessionManager`
/// per-handler-call is zero-cost.
pub struct SessionManager {
    db: PgPool,
}

impl SessionManager {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Return a reference to the underlying connection pool.
    pub fn db(&self) -> &PgPool {
        &self.db
    }

    /// Find or create a session for the given agent+user+channel.
    pub async fn get_or_create(
        &self,
        agent_id: &str,
        user_id: &str,
        channel: &str,
        dm_scope: &str,
    ) -> Result<Uuid> {
        crate::db::sessions::get_or_create_session(&self.db, agent_id, user_id, channel, dm_scope)
            .await
    }

    /// Create a brand-new session (no history reuse). Used by "New Chat".
    pub async fn create_new(
        &self,
        agent_id: &str,
        user_id: &str,
        channel: &str,
    ) -> Result<Uuid> {
        crate::db::sessions::create_new_session(&self.db, agent_id, user_id, channel).await
    }

    /// Create an isolated session. Used by cron dynamic jobs.
    pub async fn create_isolated(
        &self,
        agent_id: &str,
        user_id: &str,
        channel: &str,
    ) -> Result<Uuid> {
        crate::db::sessions::create_isolated_session_with_user(
            &self.db,
            agent_id,
            user_id,
            channel,
        )
        .await
    }

    /// Resume an existing session (updates `last_message_at`).
    pub async fn resume(&self, session_id: Uuid) -> Result<Uuid> {
        crate::db::sessions::resume_session(&self.db, session_id).await
    }

    /// Load messages for a session.
    pub async fn load_messages(
        &self,
        session_id: Uuid,
        limit: Option<i64>,
    ) -> Result<Vec<MessageRow>> {
        crate::db::sessions::load_messages(&self.db, session_id, limit).await
    }

    /// Save a message with optional tool-call metadata.
    #[allow(clippy::too_many_arguments)]
    pub async fn save_message(
        &self,
        session_id: Uuid,
        role: &str,
        content: &str,
        tool_calls: Option<&serde_json::Value>,
        tool_call_id: Option<&str>,
    ) -> Result<Uuid> {
        crate::db::sessions::save_message(
            &self.db,
            session_id,
            role,
            content,
            tool_calls,
            tool_call_id,
        )
        .await
    }

    /// Save a message with full extended metadata (multi-agent, thinking blocks).
    #[allow(clippy::too_many_arguments)]
    pub async fn save_message_ex(
        &self,
        session_id: Uuid,
        role: &str,
        content: &str,
        tool_calls: Option<&serde_json::Value>,
        tool_call_id: Option<&str>,
        sender_agent_id: Option<&str>,
        thinking_blocks: Option<&serde_json::Value>,
        parent_id: Option<Uuid>,
    ) -> Result<Uuid> {
        crate::db::sessions::save_message_ex(
            &self.db,
            session_id,
            role,
            content,
            tool_calls,
            tool_call_id,
            sender_agent_id,
            thinking_blocks,
            parent_id,
        )
        .await
    }

    /// Trim the session message history to `max` messages (oldest first).
    pub async fn trim_messages(&self, session_id: Uuid, max: u32) -> Result<u64> {
        crate::db::sessions::trim_session_messages(&self.db, session_id, max).await
    }

    /// Find the latest completed message in a session that is not a parent of
    /// any other message (the leaf of the chronologically-last chain).
    /// Returns None if the session has no completed messages yet.
    ///
    /// Used by pipeline::bootstrap as a fallback when the UI's request is
    /// missing `leaf_message_id` (stale cache / reload-during-stream), so the
    /// new user turn stays anchored to a real chain instead of orphaning.
    pub async fn latest_leaf_message_id(&self, session_id: Uuid) -> Result<Option<Uuid>> {
        use sqlx::Row;
        let row = sqlx::query(
            "SELECT m.id FROM messages m \
             WHERE m.session_id = $1 \
               AND m.status = 'complete' \
               AND NOT EXISTS ( \
                   SELECT 1 FROM messages c \
                   WHERE c.parent_message_id = m.id AND c.session_id = $1 \
               ) \
             ORDER BY m.created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(|r| r.get::<Uuid, _>("id")))
    }

    /// Log a session lifecycle event to the WAL (fire-and-forget pattern).
    pub async fn log_wal_event(
        &self,
        session_id: Uuid,
        event_type: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        crate::db::session_wal::log_event(&self.db, session_id, event_type, payload).await
    }

    /// Insert synthetic tool results for missing call IDs (crash-recovery, ENG-01).
    pub async fn insert_missing_tool_results(
        &self,
        session_id: Uuid,
        call_ids: &[String],
    ) -> Result<()> {
        crate::db::sessions::insert_missing_tool_results(&self.db, session_id, call_ids).await
    }

    /// Full-text search messages across all sessions for the given agent.
    pub async fn search_messages(
        &self,
        agent_id: &str,
        query: &str,
        limit: i64,
    ) -> Result<Vec<crate::db::sessions::SearchResult>> {
        crate::db::sessions::search_messages(&self.db, agent_id, query, limit).await
    }

    /// Get session metadata by ID.
    pub async fn get_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<crate::db::sessions::Session>> {
        crate::db::sessions::get_session(&self.db, session_id).await
    }

    /// Count messages in a session.
    pub async fn count_messages(&self, session_id: Uuid) -> Result<i64> {
        crate::db::sessions::count_messages(&self.db, session_id).await
    }

}

// ── SessionLifecycleGuard ───────────────────────────────────────────────────

/// Outcome of a session lifecycle — used by `SessionLifecycleGuard`.
pub(crate) enum SessionOutcome {
    Running,
    Done,
    Failed,
    Interrupted,
}

/// RAII guard that marks a session as `'failed'` if dropped without an explicit
/// `done()` or `fail()` call.
///
/// Usage: call `done().await` on success or `fail(reason).await` on known errors.
/// If neither is called (e.g. early `?` return), `Drop` fires a best-effort fallback
/// via `tokio::spawn` to mark the session as `'failed'`.
///
/// The guard holds `PgPool` directly (not `SessionManager`) to avoid self-referential
/// ownership issues in the `Drop` impl.
pub(crate) struct SessionLifecycleGuard {
    pub db: PgPool,
    pub session_id: Uuid,
    pub outcome: SessionOutcome,
    bg_tasks: Option<std::sync::Arc<tokio_util::task::TaskTracker>>,
    /// Agent that owns this session. Required for the Drop-path
    /// `session_failures` insert (NOT NULL column). When `None`, Drop
    /// can still mark the session as failed but skips the structured
    /// failure row (best-effort — the WAL `failed` event still records).
    agent_id: Option<String>,
    /// Set to `true` once a structured failure row has been (or will be)
    /// recorded by an explicit code path — namely:
    ///
    ///   - `fail(reason)`: caller (`finalize::finalize`) immediately spawns
    ///     `spawn_record_failure` after this call;
    ///   - `done()` / `interrupt()`: not a failure outcome, so Drop must
    ///     not synthesize one even if its in-memory transition happened to
    ///     fail at the DB level (outcome left `Running`).
    ///
    /// When `false` and Drop fires the fallback `mark-failed` path, we also
    /// enqueue a `session_failures` insert with
    /// `failure_kind = "guard_dropped"` so cancelled / early-exit sessions
    /// stop being invisible to operators (previously only `session_events`
    /// recorded "guard dropped (early exit)" while `session_failures`
    /// stayed empty).
    recorded: bool,
}

impl SessionLifecycleGuard {
    pub fn new(db: PgPool, session_id: Uuid) -> Self {
        Self {
            db,
            session_id,
            outcome: SessionOutcome::Running,
            bg_tasks: None,
            agent_id: None,
            recorded: false,
        }
    }

    pub fn with_tracker(mut self, tracker: std::sync::Arc<tokio_util::task::TaskTracker>) -> Self {
        self.bg_tasks = Some(tracker);
        self
    }

    /// Attach the owning agent's id. Required for the Drop-path
    /// `session_failures` row (`agent_id` is NOT NULL in the schema).
    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Mark session as done in DB. Sets outcome to `Done` only on DB success;
    /// on failure logs a warning and leaves `Running` so `Drop` fires fallback.
    ///
    /// Also flips `recorded = true` unconditionally so the Drop fallback never
    /// synthesizes a fake `session_failures` row for a session the caller
    /// explicitly considered successful — even if the DB transition failed.
    pub async fn done(&mut self) {
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Done, self.session_id);
        // Set BEFORE DB call: if the DB write fails the outcome stays `Running`
        // and Drop will run the mark-failed fallback, but we must not generate
        // a structured failure row for a "done" outcome.
        self.recorded = true;
        match crate::db::sessions::set_session_run_status(&self.db, self.session_id, "done").await
        {
            Ok(()) => {
                self.outcome = SessionOutcome::Done;
                if let Err(e) = crate::db::session_wal::log_event(&self.db, self.session_id, "done", None).await {
                    tracing::warn!(session_id = %self.session_id, error = %e, "failed to log WAL done event");
                }
            }
            Err(e) => tracing::warn!(
                session_id = %self.session_id,
                error = %e,
                "failed to mark session done in DB"
            ),
        }
    }

    /// Mark session as failed in DB with a reason. Sets outcome to `Failed` only on
    /// DB success; on failure logs a warning and leaves `Running` so `Drop` fires fallback.
    ///
    /// Sets `recorded = true` unconditionally because `finalize::finalize` calls
    /// `spawn_record_failure` immediately after this returns — the failure row is
    /// already in flight, so Drop must not insert another.
    pub async fn fail(&mut self, reason: &str) {
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Failed, self.session_id);
        // See doc comment above — flipping unconditionally protects against
        // a duplicate row when the DB transition fails.
        self.recorded = true;
        match crate::db::sessions::set_session_run_status(&self.db, self.session_id, "failed")
            .await
        {
            Ok(()) => {
                self.outcome = SessionOutcome::Failed;
                let payload = serde_json::json!({ "reason": reason });
                if let Err(e) = crate::db::session_wal::log_event(&self.db, self.session_id, "failed", Some(&payload)).await {
                    tracing::warn!(session_id = %self.session_id, error = %e, "failed to log WAL failed event");
                }
            }
            Err(e) => tracing::warn!(
                session_id = %self.session_id,
                error = %e,
                reason,
                "failed to mark session failed in DB"
            ),
        }
    }

    /// Mark session as interrupted (client disconnected / user cancel).
    ///
    /// Like `done()`: interrupt is not a failure, so flip `recorded = true`
    /// unconditionally to prevent the Drop fallback from synthesizing a
    /// structured failure row.
    pub async fn interrupt(&mut self, reason: &str) {
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Interrupted, self.session_id);
        self.recorded = true;
        match crate::db::sessions::set_session_run_status(&self.db, self.session_id, "interrupted").await {
            Ok(()) => {
                self.outcome = SessionOutcome::Interrupted;
                let payload = serde_json::json!({ "reason": reason });
                if let Err(e) = crate::db::session_wal::log_event(
                    &self.db, self.session_id, "interrupted", Some(&payload)
                ).await {
                    tracing::warn!(session_id = %self.session_id, error = %e,
                        "failed to log WAL interrupted event");
                }
            }
            Err(e) => tracing::warn!(
                session_id = %self.session_id, error = %e, reason,
                "failed to mark session interrupted in DB"
            ),
        }
    }
}

impl Drop for SessionLifecycleGuard {
    fn drop(&mut self) {
        if matches!(self.outcome, SessionOutcome::Running) {
            tracing::warn!(
                session_id = %self.session_id,
                "session guard dropped while still Running — spawning fallback mark-failed"
            );
            let db = self.db.clone();
            let sid = self.session_id;
            // Snapshot agent_id + recorded so the async block doesn't borrow
            // `self` after Drop returns (Drop is sync — work happens via spawn).
            let agent_id = self.agent_id.clone();
            let already_recorded = self.recorded;
            let fut = async move {
                // Conditional update: only transition `'running'` → `'failed'`.
                // The chat handler's cancel-grace path may have already written
                // `'interrupted'` before hard-aborting our task — if so, we
                // must not overwrite that signal. `rows_affected == 0` means
                // the session is already in a terminal state; skip the WAL
                // event to keep the log honest.
                match crate::db::sessions::mark_session_run_status_if_running(
                    &db,
                    sid,
                    "failed",
                )
                .await
                {
                    Ok(0) => {
                        // Already terminal — the handler (cancel-grace path)
                        // or another writer set a final status before us.
                        tracing::debug!(
                            session_id = %sid,
                            "guard drop fallback skipped: session already terminal",
                        );
                    }
                    Ok(_) => {
                        let reason = "guard dropped (early exit)";
                        let payload = serde_json::json!({ "reason": reason });
                        if let Err(e) =
                            crate::db::session_wal::log_event(&db, sid, "failed", Some(&payload))
                                .await
                        {
                            tracing::warn!(
                                error = %e,
                                session_id = %sid,
                                "failed to log WAL failed event in Drop guard"
                            );
                        }

                        // Structured `session_failures` row — best-effort.
                        // Skipped when the explicit failure path
                        // (`finalize::finalize` Failed branch) already spawned
                        // its own `spawn_record_failure`, signalled by
                        // `recorded == true` on the guard before Drop fired.
                        // Also skipped when we don't know the agent (NOT NULL
                        // column) — the WAL event still serves as a forensic
                        // breadcrumb in that case.
                        if !already_recorded {
                            if let Some(agent) = agent_id {
                                let input = crate::db::session_failures::NewSessionFailure {
                                    session_id: sid,
                                    agent_id: agent,
                                    failure_kind: "guard_dropped".to_string(),
                                    error_message: reason.to_string(),
                                    last_tool_name: None,
                                    last_tool_output: None,
                                    llm_provider: None,
                                    llm_model: None,
                                    iteration_count: None,
                                    duration_secs: None,
                                    context_json: Some(serde_json::json!({
                                        "kind": "guard_dropped",
                                        "source": "session_lifecycle_drop",
                                    })),
                                };
                                if let Err(e) = crate::db::session_failures::record_session_failure(&db, input).await {
                                    tracing::warn!(
                                        error = %e,
                                        session_id = %sid,
                                        "failed to record session_failures row from Drop guard"
                                    );
                                }
                            } else {
                                tracing::debug!(
                                    session_id = %sid,
                                    "Drop guard skipped session_failures insert: agent_id not set on guard"
                                );
                            }
                        }
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        session_id = %sid,
                        "failed to mark session as failed in Drop guard"
                    ),
                }
            };
            match &self.bg_tasks {
                Some(tracker) => { tracker.spawn(fut); }
                None => { tokio::spawn(fut); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn lifecycle_guard_interrupt_writes_wal(pool: sqlx::PgPool) {
        let session_id = crate::db::sessions::create_new_session(&pool, "test-agent", "test-user", "test-channel")
            .await
            .unwrap();

        let mut guard = SessionLifecycleGuard::new(pool.clone(), session_id);
        guard.interrupt("sink_closed").await;

        let status: String = sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "interrupted");

        let event_type: String = sqlx::query_scalar(
            "SELECT event_type FROM session_events WHERE session_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event_type, "interrupted");
    }

    #[tokio::test]
    async fn lifecycle_guard_with_tracker_uses_tracker_on_drop() {
        use std::sync::Arc;
        use tokio_util::task::TaskTracker;

        let tracker = Arc::new(TaskTracker::new());
        let db = sqlx::PgPool::connect_lazy("postgres://invalid").unwrap();
        let guard = SessionLifecycleGuard::new(db, uuid::Uuid::new_v4())
            .with_tracker(tracker.clone());
        // Guard is still Running → Drop will call tracker.spawn(...)
        // tracker must not be closed yet for spawn to succeed.
        assert!(!tracker.is_closed());
        drop(guard);
        // After drop, one task was submitted to the tracker.
        assert!(!tracker.is_empty());
    }

    /// Drop on a still-Running guard with `with_agent` set inserts a
    /// `session_failures` row with `failure_kind = 'guard_dropped'` —
    /// covering the "cancelled mid-stream / SSE disconnect" path that
    /// previously left `session_failures` empty.
    #[sqlx::test(migrations = "../../migrations")]
    async fn lifecycle_guard_drop_records_session_failure(pool: sqlx::PgPool) {
        use std::sync::Arc;
        use tokio_util::task::TaskTracker;

        let session_id = crate::db::sessions::create_new_session(
            &pool,
            "test-agent",
            "test-user",
            "test-channel",
        )
        .await
        .unwrap();
        // Drop's fallback only fires when the session is currently 'running'
        // (mark_session_run_status_if_running). create_new_session leaves
        // run_status NULL, so we must transition to 'running' first to
        // simulate the live-stream state we're emulating cancellation against.
        crate::db::sessions::set_session_run_status(&pool, session_id, "running")
            .await
            .unwrap();

        let tracker = Arc::new(TaskTracker::new());
        {
            // Build the guard, attach agent + tracker, then drop without
            // calling done/fail/interrupt — simulates cancellation / early-exit.
            let _guard = SessionLifecycleGuard::new(pool.clone(), session_id)
                .with_tracker(tracker.clone())
                .with_agent("test-agent");
        } // <-- Drop fires here

        // Wait for the spawned task to flush.
        tracker.close();
        tracker.wait().await;

        // 1. Session marked failed.
        let status: String =
            sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
                .bind(session_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");

        // 2. WAL event recorded.
        let event_type: String = sqlx::query_scalar(
            "SELECT event_type FROM session_events WHERE session_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event_type, "failed");

        // 3. session_failures row inserted with the right shape.
        let rows = crate::db::session_failures::get_session_failures_for_session(&pool, session_id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "exactly one failure row recorded");
        let row = &rows[0];
        assert_eq!(row.failure_kind, "guard_dropped");
        assert_eq!(row.agent_id, "test-agent");
        assert_eq!(row.error_message, "guard dropped (early exit)");
    }

    /// After `fail(reason)` the `recorded` flag is set, so even if Drop fires
    /// the fallback (e.g. because the DB transition failed and outcome stayed
    /// Running) it must NOT insert a duplicate `session_failures` row — the
    /// caller (`finalize::finalize`) is responsible for that path via
    /// `spawn_record_failure`.
    #[sqlx::test(migrations = "../../migrations")]
    async fn lifecycle_guard_fail_then_drop_does_not_duplicate(pool: sqlx::PgPool) {
        use std::sync::Arc;
        use tokio_util::task::TaskTracker;

        let session_id = crate::db::sessions::create_new_session(
            &pool,
            "test-agent",
            "test-user",
            "test-channel",
        )
        .await
        .unwrap();

        let tracker = Arc::new(TaskTracker::new());
        {
            let mut guard = SessionLifecycleGuard::new(pool.clone(), session_id)
                .with_tracker(tracker.clone())
                .with_agent("test-agent");
            guard.fail("explicit failure").await;
            // Drop fires here — but recorded == true means no extra row.
        }

        tracker.close();
        tracker.wait().await;

        let rows = crate::db::session_failures::get_session_failures_for_session(&pool, session_id)
            .await
            .unwrap();
        assert!(
            rows.is_empty(),
            "explicit fail() leaves session_failures recording to finalize::spawn_record_failure; Drop must not double-insert (got {} rows)",
            rows.len()
        );
    }

    /// `done()` and `interrupt()` are not failures: even if Drop's fallback
    /// path were to fire (it won't here because outcome != Running after a
    /// successful transition), the `recorded` flag would still suppress an
    /// erroneous failure row.
    #[sqlx::test(migrations = "../../migrations")]
    async fn lifecycle_guard_done_drop_records_no_failure(pool: sqlx::PgPool) {
        use std::sync::Arc;
        use tokio_util::task::TaskTracker;

        let session_id = crate::db::sessions::create_new_session(
            &pool,
            "test-agent",
            "test-user",
            "test-channel",
        )
        .await
        .unwrap();

        let tracker = Arc::new(TaskTracker::new());
        {
            let mut guard = SessionLifecycleGuard::new(pool.clone(), session_id)
                .with_tracker(tracker.clone())
                .with_agent("test-agent");
            guard.done().await;
        }

        tracker.close();
        tracker.wait().await;

        let rows = crate::db::session_failures::get_session_failures_for_session(&pool, session_id)
            .await
            .unwrap();
        assert!(rows.is_empty(), "done() must not produce a failure row");
    }
}

