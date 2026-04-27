//! Single exit point for pipeline::execute — persists final/partial message,
//! transitions SessionLifecycleGuard, enqueues knowledge extraction.
//!
//! See docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md §4.

use crate::agent::memory_service::MemoryService;
use crate::agent::pipeline::sink::{EventSink, PipelineEvent};
use crate::agent::providers::LlmProvider;
use crate::agent::session_manager::{SessionLifecycleGuard, SessionManager};
use crate::agent::stream_event::StreamEvent;
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

// ── UI notifications ──────────────────────────────────────────────────────────
//
// Notifications are DB-persisted + WS-broadcast. The sidebar/bell icon in the UI
// relies on them for agent lifecycle signals (error, iteration limit, loop). Old
// engine_sse.rs / engine_execution.rs emitted these at trigger sites via the now-
// deleted `pipeline::execution::notify_*` helpers. We restore them here so the
// pipeline path has parity with the pre-refactor behaviour.

/// Spawn a DB-persisted notification that the agent run failed.
pub(crate) fn notify_agent_error(
    db: PgPool,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    agent_name: &str,
    reason: &str,
    tracker: &TaskTracker,
) {
    if let Some(ui_tx) = ui_event_tx {
        let tx = ui_tx.clone();
        let agent_name = agent_name.to_string();
        let reason = reason.to_string();
        tracker.spawn(async move {
            let _ = crate::gateway::notify(
                &db,
                &tx,
                "agent_error",
                "Agent Error",
                &format!("Agent {agent_name} run failed: {reason}"),
                serde_json::json!({"agent": agent_name, "reason": reason}),
            )
            .await;
        });
    }
}

/// Spawn a DB-persisted notification that the agent hit the turn/iteration limit.
pub(crate) fn notify_iteration_limit(
    db: PgPool,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    agent_name: &str,
    max_iterations: usize,
    tracker: &TaskTracker,
) {
    tracing::warn!(
        agent = %agent_name,
        max_iterations,
        "agent reached iteration limit"
    );
    if let Some(ui_tx) = ui_event_tx {
        let tx = ui_tx.clone();
        let agent_name = agent_name.to_string();
        tracker.spawn(async move {
            let _ = crate::gateway::notify(
                &db,
                &tx,
                "iteration_limit",
                &format!("Iteration limit: {agent_name}"),
                &format!(
                    "Agent {agent_name} reached its iteration limit ({max_iterations} iterations). The task may be incomplete."
                ),
                serde_json::json!({
                    "agent": agent_name,
                    "max_iterations": max_iterations,
                }),
            )
            .await;
        });
    }
}

/// Spawn a DB-persisted notification that the agent was stopped after detecting a loop.
pub(crate) fn notify_loop_detected(
    db: PgPool,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    agent_name: &str,
    session_id: Uuid,
    tracker: &TaskTracker,
) {
    if let Some(ui_tx) = ui_event_tx {
        let tx = ui_tx.clone();
        let agent_name = agent_name.to_string();
        tracker.spawn(async move {
            let _ = crate::gateway::notify(
                &db,
                &tx,
                "agent_loop_detected",
                &format!("Agent stuck in loop: {agent_name}"),
                &format!(
                    "Agent {agent_name} was stopped after detecting a repeating pattern. Session: {session_id}"
                ),
                serde_json::json!({
                    "agent": agent_name,
                    "session_id": session_id.to_string(),
                }),
            )
            .await;
        });
    }
}

// ── FinalizeOutcome ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum FinalizeOutcome {
    Done {
        assistant_text: String,
        /// Thinking blocks JSON to be persisted with the message.
        thinking_json: Option<serde_json::Value>,
    },
    Failed {
        partial: String,
        reason: String,
    },
    Interrupted {
        partial: String,
        reason: &'static str,
    },
}

// ── FinalizeContext ───────────────────────────────────────────────────────────

pub struct FinalizeContext {
    pub db: PgPool,
    pub session_id: Uuid,
    pub agent_name: String,
    pub message_count: usize,
    pub provider: Arc<dyn LlmProvider>,
    pub memory_store: Arc<dyn MemoryService>,
    /// Parent id threaded from bootstrap's user-message save; used as
    /// `parent_message_id` for the assistant reply so reload-from-active-path
    /// finds both sides of the turn.
    pub user_message_id: Option<Uuid>,
    /// Broadcast channel used to push DB-persisted notifications (agent_error,
    /// iteration_limit, loop_detected) to the UI. `None` means notifications
    /// are disabled (e.g. in unit tests with no UI).
    pub ui_event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    /// Max iterations configured for this agent; used when surfacing an
    /// `iteration_limit` notification to UI.
    pub max_iterations: usize,
    /// Shared task tracker for fire-and-forget spawns (notifications, knowledge
    /// extraction). Ensures graceful shutdown waits for them.
    pub bg_tasks: Arc<TaskTracker>,
}

// ── finalize() ────────────────────────────────────────────────────────────────

/// Persist the final (or partial) assistant message, transition the lifecycle
/// guard, and (on `Done`) spawn knowledge extraction in the background.
///
/// Returns the saved assistant text so callers can pass it upstream.
pub async fn finalize<S: EventSink>(
    ctx: FinalizeContext,
    outcome: FinalizeOutcome,
    sink: &mut S,
    lifecycle_guard: &mut SessionLifecycleGuard,
) -> anyhow::Result<String> {
    let sm = SessionManager::new(ctx.db.clone());
    let agent_name_ref = ctx.agent_name.as_str();

    let out = match &outcome {
        FinalizeOutcome::Done { assistant_text, thinking_json } => {
            // Resolve the guard before the DB write (C2): if save_message fails the
            // session must not be marked 'failed' by guard Drop — the LLM already
            // produced its response. Failure to persist is non-fatal for session status.
            lifecycle_guard.done().await;
            if let Err(e) = sm.save_message_ex(
                ctx.session_id,
                "assistant",
                assistant_text,
                None,
                None,
                Some(agent_name_ref),
                thinking_json.as_ref(),
                ctx.user_message_id,
            )
            .await
            {
                tracing::error!(
                    session_id = %ctx.session_id,
                    error = %e,
                    "finalize: failed to persist assistant message"
                );
            }
            spawn_knowledge_extraction(
                ctx.db.clone(),
                ctx.session_id,
                ctx.agent_name.clone(),
                ctx.provider.clone(),
                ctx.memory_store.clone(),
                ctx.message_count,
                &ctx.bg_tasks,
            );
            assistant_text.clone()
        }
        FinalizeOutcome::Failed { partial, reason } => {
            if !partial.is_empty() {
                let _ = sm
                    .save_message_ex(
                        ctx.session_id,
                        "assistant",
                        partial,
                        None,
                        None,
                        Some(agent_name_ref),
                        None,
                        ctx.user_message_id,
                    )
                    .await;
            }
            lifecycle_guard.fail(reason).await;
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::Error(reason.clone())))
                .await;
            // UI notification (DB + WS broadcast) — surfaces the failure in the bell
            // icon + notification list. Specialized reasons get their own notification
            // kind (loop_detected, iteration_limit) rather than the generic agent_error.
            let lowered = reason.to_ascii_lowercase();
            if lowered.starts_with("loop_detected") {
                notify_loop_detected(
                    ctx.db.clone(),
                    ctx.ui_event_tx.as_ref(),
                    &ctx.agent_name,
                    ctx.session_id,
                    &ctx.bg_tasks,
                );
            } else if lowered.starts_with("iteration_limit") {
                notify_iteration_limit(
                    ctx.db.clone(),
                    ctx.ui_event_tx.as_ref(),
                    &ctx.agent_name,
                    ctx.max_iterations,
                    &ctx.bg_tasks,
                );
            } else {
                notify_agent_error(
                    ctx.db.clone(),
                    ctx.ui_event_tx.as_ref(),
                    &ctx.agent_name,
                    reason,
                    &ctx.bg_tasks,
                );
            }
            partial.clone()
        }
        FinalizeOutcome::Interrupted { partial, reason } => {
            if !partial.is_empty() {
                let _ = sm
                    .save_message_ex(
                        ctx.session_id,
                        "assistant",
                        partial,
                        None,
                        None,
                        Some(agent_name_ref),
                        None,
                        ctx.user_message_id,
                    )
                    .await;
            }
            lifecycle_guard.interrupt(reason).await;
            partial.clone()
        }
    };

    Ok(out)
}

// ── finalize_context_from_engine() ───────────────────────────────────────────

/// Construct a `FinalizeContext` from an `AgentEngine` reference.
pub fn finalize_context_from_engine(
    engine: &crate::agent::engine::AgentEngine,
    session_id: Uuid,
    message_count: usize,
    user_message_id: Option<Uuid>,
) -> FinalizeContext {
    FinalizeContext {
        db: engine.cfg().db.clone(),
        session_id,
        agent_name: engine.cfg().agent.name.clone(),
        message_count,
        provider: engine.cfg().provider.clone(),
        memory_store: engine.cfg().memory_store.clone(),
        user_message_id,
        ui_event_tx: engine.state().ui_event_tx.clone(),
        max_iterations: engine.tool_loop_config().effective_max_iterations(),
        bg_tasks: engine.state().bg_tasks.clone(),
    }
}

// ── spawn_knowledge_extraction() ─────────────────────────────────────────────

pub(crate) fn spawn_knowledge_extraction(
    db: PgPool,
    session_id: Uuid,
    agent_name: String,
    provider: Arc<dyn LlmProvider>,
    memory_store: Arc<dyn MemoryService>,
    message_count: usize,
    tracker: &TaskTracker,
) {
    if message_count >= 5 {
        tracker.spawn(async move {
            crate::agent::knowledge_extractor::extract_and_save(
                db, session_id, agent_name, provider, memory_store,
            )
            .await;
        });
    }
}

// ── execute_status_to_finalize() ─────────────────────────────────────────────

/// Convert [`ExecuteStatus`] + (final_text, thinking_json) into [`FinalizeOutcome`].
///
/// Used by the thin adapter methods in Tasks 7/8/9.
pub fn execute_status_to_finalize(
    status: crate::agent::pipeline::execute::ExecuteStatus,
    final_text: String,
    thinking_json: Option<serde_json::Value>,
) -> FinalizeOutcome {
    use crate::agent::pipeline::execute::ExecuteStatus;
    match status {
        ExecuteStatus::Done => FinalizeOutcome::Done {
            assistant_text: final_text,
            thinking_json,
        },
        ExecuteStatus::Failed(reason) => FinalizeOutcome::Failed {
            partial: final_text,
            reason,
        },
        ExecuteStatus::Interrupted(reason) => FinalizeOutcome::Interrupted {
            partial: final_text,
            reason,
        },
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::pipeline::sink::test_support::MockSink;
    use async_trait::async_trait;

    // ── Minimal stubs for LlmProvider and MemoryService that panic on use ──
    // These exist so FinalizeContext can be constructed. The Failed and
    // Interrupted paths in finalize() do NOT call the provider or memory
    // store, so panic-on-call is safe. Done path is covered by integration
    // tests run on CI (not here, because cargo test is broken locally).

    struct NeverCalledProvider;
    #[async_trait]
    impl LlmProvider for NeverCalledProvider {
        async fn chat(
            &self,
            _messages: &[hydeclaw_types::Message],
            _tools: &[hydeclaw_types::ToolDefinition],
        ) -> anyhow::Result<hydeclaw_types::LlmResponse> {
            panic!("not called in Failed/Interrupted path")
        }

        fn name(&self) -> &str {
            "never-called"
        }
    }

    struct NeverCalledMemory;
    #[async_trait]
    impl MemoryService for NeverCalledMemory {
        fn is_available(&self) -> bool {
            false
        }

        async fn search(
            &self,
            _query: &str,
            _limit: usize,
            _exclude_ids: &[String],
            _agent_id: &str,
        ) -> anyhow::Result<(Vec<crate::memory::MemoryResult>, String)> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn index(
            &self,
            _content: &str,
            _source: &str,
            _pinned: bool,
            _scope: &str,
            _agent_id: &str,
        ) -> anyhow::Result<String> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn index_batch(
            &self,
            _items: &[(String, String, bool, String)],
            _agent_id: &str,
        ) -> anyhow::Result<Vec<String>> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn load_pinned(
            &self,
            _agent_id: &str,
            _budget_tokens: u32,
        ) -> anyhow::Result<(String, Vec<String>)> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn get(
            &self,
            _chunk_id: Option<&str>,
            _source: Option<&str>,
            _limit: usize,
        ) -> anyhow::Result<Vec<crate::memory::MemoryChunk>> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn delete(&self, _chunk_id: &str) -> anyhow::Result<bool> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn recent(&self, _limit: i64) -> anyhow::Result<Vec<crate::memory::MemoryResult>> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn wipe_agent_memory(&self, _agent_id: &str) -> anyhow::Result<u64> {
            panic!("not called in Failed/Interrupted path")
        }

        async fn enqueue_reindex_task(
            &self,
            _params: serde_json::Value,
        ) -> anyhow::Result<uuid::Uuid> {
            panic!("not called in Failed/Interrupted path")
        }
    }

    fn build_ctx(db: PgPool, session_id: Uuid) -> FinalizeContext {
        FinalizeContext {
            db,
            session_id,
            agent_name: "test-agent".to_string(),
            message_count: 0,
            provider: Arc::new(NeverCalledProvider),
            memory_store: Arc::new(NeverCalledMemory),
            user_message_id: None,
            // No UI in unit tests — notify_* becomes a no-op with ui_event_tx=None.
            ui_event_tx: None,
            max_iterations: 0,
            bg_tasks: Arc::new(TaskTracker::new()),
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn finalize_failed_emits_error_and_saves_partial(pool: PgPool) {
        let session_id =
            crate::db::sessions::create_new_session(&pool, "test-agent", "test-user", "test-channel")
                .await
                .unwrap();

        let ctx = build_ctx(pool.clone(), session_id);
        let mut guard = SessionLifecycleGuard::new(pool.clone(), session_id);
        let mut sink = MockSink::new();

        let text = finalize(
            ctx,
            FinalizeOutcome::Failed {
                partial: "partial".into(),
                reason: "llm_exhausted".into(),
            },
            &mut sink,
            &mut guard,
        )
        .await
        .unwrap();

        assert_eq!(text, "partial");
        assert!(
            sink.events
                .iter()
                .any(|e| matches!(e, PipelineEvent::Stream(StreamEvent::Error(_)))),
            "Error event emitted"
        );
        let role: String = sqlx::query_scalar(
            "SELECT role FROM messages WHERE session_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(role, "assistant", "partial saved as assistant message");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn finalize_interrupted_does_not_emit_error(pool: PgPool) {
        let session_id =
            crate::db::sessions::create_new_session(&pool, "test-agent", "test-user", "test-channel")
                .await
                .unwrap();

        let ctx = build_ctx(pool.clone(), session_id);
        let mut guard = SessionLifecycleGuard::new(pool.clone(), session_id);
        let mut sink = MockSink::new();

        finalize(
            ctx,
            FinalizeOutcome::Interrupted {
                partial: "p".into(),
                reason: "sink_closed",
            },
            &mut sink,
            &mut guard,
        )
        .await
        .unwrap();

        assert!(
            !sink
                .events
                .iter()
                .any(|e| matches!(e, PipelineEvent::Stream(StreamEvent::Error(_)))),
            "no Error event on interrupt"
        );
    }
}
