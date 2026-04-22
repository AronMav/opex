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
}

impl SessionLifecycleGuard {
    pub fn new(db: PgPool, session_id: Uuid) -> Self {
        Self { db, session_id, outcome: SessionOutcome::Running, bg_tasks: None }
    }

    pub fn with_tracker(mut self, tracker: std::sync::Arc<tokio_util::task::TaskTracker>) -> Self {
        self.bg_tasks = Some(tracker);
        self
    }

    /// Mark session as done in DB. Sets outcome to `Done` only on DB success;
    /// on failure logs a warning and leaves `Running` so `Drop` fires fallback.
    pub async fn done(&mut self) {
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Done, self.session_id);
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
    pub async fn fail(&mut self, reason: &str) {
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Failed, self.session_id);
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
    pub async fn interrupt(&mut self, reason: &str) {
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Interrupted, self.session_id);
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
                        let payload =
                            serde_json::json!({ "reason": "guard dropped (early exit)" });
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
}

