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
use opex_db::sessions::warn_invalid_transition;
use opex_db::SessionStatus;

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

    /// Find or create a session for the given agent+user+channel(+chat_scope).
    /// Returns `(session_id, reentry_mode)` so bootstrap can decide whether
    /// to warm the LoopDetector from the timeline.
    ///
    /// `chat_scope` (T03 triage Point 5): per-chat/group disambiguator
    /// extracted from the incoming message's adapter context (e.g. Telegram
    /// `chat_id`). Without it, all chats/groups on the same platform for the
    /// same user_id collapse into one session — a cross-chat context leak.
    pub async fn get_or_create(
        &self,
        agent_id: &str,
        user_id: &str,
        channel: &str,
        dm_scope: &str,
        chat_scope: Option<&str>,
    ) -> Result<(Uuid, opex_db::ReentryMode)> {
        crate::db::sessions::get_or_create_session(&self.db, agent_id, user_id, channel, dm_scope, chat_scope)
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
    ///
    /// Currently unused — the legacy `engine/stream.rs::handle_isolated()`
    /// caller was migrated to detached `spawn_persist_assistant_message`
    /// (closes a cancellation gap on intermediate-assistant rows). Kept as a
    /// public API surface for parity with `save_message_ex` so external
    /// callers / future paths don't have to thread the extended metadata.
    #[allow(clippy::too_many_arguments, dead_code)]
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

    /// Log a session lifecycle event to the timeline (fire-and-forget pattern).
    pub async fn log_timeline_event(
        &self,
        session_id: Uuid,
        event_type: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        crate::db::session_timeline::log_event(&self.db, session_id, event_type, payload).await
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
        crate::db::sessions::search_messages(&self.db, Some(agent_id), query, limit).await
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

/// RAII guard that marks a session as `'interrupted'` (or `'failed'` if a
/// genuine failure was already recorded) if dropped without an explicit
/// `done()` or `fail()` call.
///
/// Usage: call `done().await` on success or `fail(reason).await` on known errors.
/// If neither is called (e.g. early `?` return, panic, abort), `Drop` fires a
/// best-effort fallback via `tokio::spawn` to mark the session terminal.
/// Per invariant G1, a session only ends `'failed'` when a genuine
/// engine/LLM error was recorded (`recorded == true`); every other forced
/// termination claims `'interrupted'` so the session stays resumable.
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
    /// failure row (best-effort — the timeline `failed` event still records).
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
    /// stop being invisible to operators (previously only `session_timeline`
    /// recorded "guard dropped (early exit)" while `session_failures`
    /// stayed empty).
    recorded: bool,
    /// SSE-only: the `stream_jobs.id` for THIS turn's stream, set by
    /// `execute_sse` after the POST handler registers the stream. When present,
    /// every terminal `run_status` write (`done`/`fail`/`interrupt` and the
    /// Drop backstop) is ownership-gated: if a NEWER stream job exists for the
    /// same session (`stream_jobs::is_superseded`), this turn has been
    /// superseded by a later same-session `POST /api/chat` that already
    /// re-claimed the session `running`, so we MUST NOT flip the row terminal
    /// — doing so would strand the newer, still-running turn at that terminal
    /// status (the T2 critical clobber). `None` for channel/cron/retry paths,
    /// which never share a session row with a concurrent registry supersede.
    stream_job_id: Option<Uuid>,
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
            stream_job_id: None,
        }
    }

    pub fn with_tracker(mut self, tracker: std::sync::Arc<tokio_util::task::TaskTracker>) -> Self {
        self.bg_tasks = Some(tracker);
        self
    }

    /// Attach THIS turn's `stream_jobs.id` so terminal `run_status` writes are
    /// ownership-gated against same-session supersede (T2). `execute_sse` calls
    /// this after the POST handler registers the stream. See the `stream_job_id`
    /// field doc for the invariant this protects.
    pub(crate) fn set_stream_job_id(&mut self, id: Option<Uuid>) {
        self.stream_job_id = id;
    }

    /// True when this turn has been superseded by a newer same-session stream —
    /// its `stream_jobs` row is older than a later turn's. Only meaningful on
    /// the SSE path (`stream_job_id` set); `None` (channel/cron/retry) is never
    /// superseded. Fail-open (`false`) on a DB error so a transient blip cannot
    /// silently swallow a legitimate terminal write.
    async fn superseded_by_newer_turn(&self) -> bool {
        match self.stream_job_id {
            Some(job_id) => {
                match crate::gateway::stream_jobs::is_superseded(&self.db, job_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            session_id = %self.session_id,
                            error = %e,
                            "lifecycle guard: supersede check failed — treating as not superseded"
                        );
                        false
                    }
                }
            }
            None => false,
        }
    }

    /// Shared guard for `done`/`fail`/`interrupt`: if this turn was superseded
    /// by a newer same-session stream, resolve the guard as a DB no-op and
    /// report `true` so the caller returns before touching `run_status`.
    ///
    /// We mark the guard `recorded = true` and flip `outcome` out of `Running`
    /// so the Drop backstop ALSO stays quiet (no fallback claim, no synthesized
    /// `session_failures` row) — the newer turn is the sole owner of the
    /// session row and will finalize it. Returns `false` (proceed normally) for
    /// the common, non-superseded case.
    async fn skip_terminal_write_if_superseded(&mut self, phase: &str) -> bool {
        if !self.superseded_by_newer_turn().await {
            return false;
        }
        tracing::debug!(
            session_id = %self.session_id,
            phase,
            "lifecycle guard: skipping terminal run_status write — turn superseded by a newer same-session stream"
        );
        // Resolved-as-superseded: suppress the Drop backstop. We intentionally
        // leave the DB row untouched (the newer turn owns it) but record the
        // in-memory outcome as terminal so `Drop` sees a non-`Running` state.
        self.recorded = true;
        self.outcome = SessionOutcome::Interrupted;
        true
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
        if self.skip_terminal_write_if_superseded("done").await {
            return;
        }
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Done, self.session_id);
        // Set BEFORE DB call: if the DB write fails the outcome stays `Running`
        // and Drop will run the mark-failed fallback, but we must not generate
        // a structured failure row for a "done" outcome.
        self.recorded = true;
        match crate::db::sessions::set_session_run_status(&self.db, self.session_id, "done").await
        {
            Ok(()) => {
                self.outcome = SessionOutcome::Done;
                if let Err(e) = crate::db::session_timeline::log_event(&self.db, self.session_id, "done", None).await {
                    tracing::warn!(session_id = %self.session_id, error = %e, "failed to log timeline done event");
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
        if self.skip_terminal_write_if_superseded("fail").await {
            return;
        }
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
                if let Err(e) = crate::db::session_timeline::log_event(&self.db, self.session_id, "failed", Some(&payload)).await {
                    tracing::warn!(session_id = %self.session_id, error = %e, "failed to log timeline failed event");
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
        if self.skip_terminal_write_if_superseded("interrupt").await {
            return;
        }
        warn_invalid_transition(Some(SessionStatus::Running), SessionStatus::Interrupted, self.session_id);
        self.recorded = true;
        match crate::db::sessions::set_session_run_status(&self.db, self.session_id, "interrupted").await {
            Ok(()) => {
                self.outcome = SessionOutcome::Interrupted;
                let payload = serde_json::json!({ "reason": reason });
                if let Err(e) = crate::db::session_timeline::log_event(
                    &self.db, self.session_id, "interrupted", Some(&payload)
                ).await {
                    tracing::warn!(session_id = %self.session_id, error = %e,
                        "failed to log timeline interrupted event");
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
            // The guard sees `Running` at drop. This is the EXPECTED state for
            // client-gone / cancel-grace interrupts — those paths mark the DB
            // directly (not via the guard), so warning up front produced a
            // misleading "fallback mark-failed" line on every normal interrupt.
            // The atomic running→failed claim below is idempotent; we log per
            // outcome instead: a genuine early exit (Ok(true)) warns, a benign
            // no-op (Ok(false), already terminal) stays at debug.
            let db = self.db.clone();
            let sid = self.session_id;
            // Snapshot agent_id + recorded + stream_job_id so the async block
            // doesn't borrow `self` after Drop returns (Drop is sync — work
            // happens via spawn).
            let agent_id = self.agent_id.clone();
            let already_recorded = self.recorded;
            let stream_job_id = self.stream_job_id;
            // G1: an unrecorded early exit (abort, panic, transport death) is a
            // forced termination, not an engine error — claim `interrupted` so
            // the session stays resumable. Only a recorded genuine failure
            // (explicit `fail()` path already finalized/finalizing) keeps
            // `failed`.
            let claim_status = if already_recorded { "failed" } else { "interrupted" };
            let fut = async move {
                // T2 ownership gate for the hard-abort path (no finalize ran, so
                // the skip in done/fail/interrupt never fired): if a newer
                // same-session stream superseded this turn, the newer turn owns
                // the `running` row — the atomic running→{claim_status} claim
                // below WOULD succeed and clobber it. Skip entirely.
                if let Some(job_id) = stream_job_id
                    && crate::gateway::stream_jobs::is_superseded(&db, job_id).await.unwrap_or(false)
                {
                    tracing::debug!(
                        session_id = %sid,
                        "Drop guard cleanup skipped: turn superseded by a newer same-session stream"
                    );
                    return;
                }
                // Unified cleanup path (I1). Idempotent — returns Ok(false)
                // if another writer (finalize, watchdog, cancel-grace) already
                // finalized the session. `cleanup_session_terminated` performs
                // the atomic running→{claim_status} claim, streaming-message
                // preservation, synthetic tool-result patching, and the
                // matching timeline event in one transaction; no manual
                // `log_event` is needed after.
                match crate::db::sessions::cleanup_session_terminated(
                    &db,
                    sid,
                    claim_status,
                    "guard dropped (early exit)",
                )
                .await
                {
                    Ok(true) => {
                        // We won the running→{claim_status} claim, so no other
                        // writer finalized this session — a genuine early exit
                        // (panic / `?` bubble / abort) worth surfacing.
                        tracing::warn!(
                            session_id = %sid,
                            "session guard dropped while Running — marked session {} (early exit)",
                            claim_status
                        );
                        // Structured `session_failures` row — best-effort.
                        // Skipped when the explicit failure path
                        // (`finalize::finalize` Failed branch) already spawned
                        // its own `spawn_record_failure`, signalled by
                        // `recorded == true` on the guard before Drop fired.
                        // Also skipped when we don't know the agent (NOT NULL
                        // column) — the timeline event still serves as a forensic
                        // breadcrumb in that case.
                        if !already_recorded {
                            if let Some(agent) = agent_id {
                                let input = crate::db::session_failures::NewSessionFailure {
                                    session_id: sid,
                                    agent_id: agent,
                                    failure_kind: "guard_dropped".to_string(),
                                    error_message: "guard dropped (early exit)".to_string(),
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
                    Ok(false) => {
                        // Already terminal — finalize / watchdog / cancel-grace
                        // wrote a final status before us. Stay quiet.
                        tracing::debug!(
                            session_id = %sid,
                            "Drop guard cleanup no-op: session already terminal",
                        );
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        session_id = %sid,
                        "Drop guard cleanup failed"
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

    /// Create a fresh session and transition it straight to `running` —
    /// `create_new_session` leaves `run_status` NULL, so `Drop`'s fallback
    /// (which only claims `WHERE run_status = 'running'`) would otherwise
    /// never fire for these tests.
    async fn create_running_session(pool: &sqlx::PgPool) -> Uuid {
        let session_id = crate::db::sessions::create_new_session(
            pool,
            "test-agent",
            "test-user",
            "test-channel",
        )
        .await
        .unwrap();
        crate::db::sessions::set_session_run_status(pool, session_id, "running")
            .await
            .unwrap();
        session_id
    }

    /// Poll `sessions.run_status` until it reaches a terminal value.
    /// `Drop`'s fallback cleanup is spawned via `tokio::spawn` (fire-and-forget)
    /// when no `TaskTracker` is attached, so tests without `with_tracker` must
    /// poll rather than await a tracker.
    async fn wait_for_terminal_status(pool: &sqlx::PgPool, session_id: Uuid) -> String {
        const TERMINAL: &[&str] = &["done", "failed", "interrupted", "cancelled", "timeout"];
        for _ in 0..100 {
            let status: Option<String> =
                sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
                    .bind(session_id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if let Some(s) = &status {
                if TERMINAL.contains(&s.as_str()) {
                    return s.clone();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("session {session_id} did not reach a terminal run_status in time");
    }

    /// G1: a guard dropped while `Running` with no recorded failure (abort,
    /// panic, transport death) must claim `interrupted`, not `failed` — the
    /// session must stay resumable. The forensic `session_failures` row
    /// (kind `guard_dropped`) is still written exactly as before.
    #[sqlx::test(migrations = "../../migrations")]
    async fn guard_drop_without_recorded_error_marks_interrupted(pool: sqlx::PgPool) {
        let session_id = create_running_session(&pool).await;
        {
            let _guard = SessionLifecycleGuard::new(pool.clone(), session_id)
                .with_agent("TestAgent");
            // dropped here while outcome == Running, recorded == false
        }
        // Drop spawns async cleanup — poll until terminal.
        let status = wait_for_terminal_status(&pool, session_id).await;
        assert_eq!(
            status, "interrupted",
            "unrecorded early exit must be resumable, not failed"
        );
        // Forensic row still written.
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT failure_kind FROM session_failures WHERE session_id = $1",
        )
        .bind(session_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(kind.as_deref(), Some("guard_dropped"));
    }

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
            "SELECT event_type FROM session_timeline WHERE session_id = $1 ORDER BY created_at DESC LIMIT 1",
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
    /// previously left `session_failures` empty. Per G1, an unrecorded
    /// drop claims `interrupted` (resumable), not `failed` — only the
    /// forensic `session_failures` row still records the event.
    #[sqlx::test(migrations = "../../migrations")]
    async fn lifecycle_guard_drop_records_session_failure(pool: sqlx::PgPool) {
        use std::sync::Arc;
        use tokio_util::task::TaskTracker;

        let session_id = create_running_session(&pool).await;

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

        // 1. Session marked interrupted (G1: unrecorded drop is resumable,
        // not a failure).
        let status: String =
            sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
                .bind(session_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "interrupted");

        // 2. Timeline event recorded.
        let event_type: String = sqlx::query_scalar(
            "SELECT event_type FROM session_timeline WHERE session_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event_type, "interrupted");

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

