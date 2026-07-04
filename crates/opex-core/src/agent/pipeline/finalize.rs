//! Single exit point for pipeline::execute — persists final/partial message,
//! transitions SessionLifecycleGuard, enqueues knowledge extraction.
//!
//! See docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md §4.

use crate::agent::clarify_manager::ClarifyManager;
use crate::agent::memory_service::MemoryService;
use crate::agent::pipeline::sink::{EventSink, PipelineEvent};
use crate::agent::providers::LlmProvider;
use crate::agent::session_manager::{SessionLifecycleGuard, SessionManager, SessionOutcome};
use crate::agent::stream_event::StreamEvent;
use crate::db::session_failures::{record_session_failure, NewSessionFailure};
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

// ── Failure classification + recording ────────────────────────────────────────

/// Classify a failure reason string into a stable `failure_kind` enum value.
///
/// The values are free-form `TEXT` in the DB but the writer sticks to this
/// enum so dashboards / filters can group consistently.
pub(crate) fn classify_failure_kind(reason: &str) -> &'static str {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("guard dropped") || lower.contains("early exit") {
        // Synthesized by `SessionLifecycleGuard::Drop` when the engine task
        // exited without reaching `lifecycle_guard.fail()` (cancellation,
        // SSE disconnect, internal timeout cascading, panic-in-tokio-spawn).
        "guard_dropped"
    } else if lower.contains("did not complete within") || lower.contains("timed out waiting") {
        "sub_agent_timeout"
    } else if lower.starts_with("loop_detected")
        || lower.contains("loop_detected")
        || lower.contains("max loop nudges")
        || lower.contains("failed") && lower.contains("times consecutively")
    {
        "tool_error"
    } else if lower.starts_with("iteration_limit")
        || lower.contains("max iterations")
        || lower.contains("turn limit")
    {
        "max_iterations"
    } else if lower.contains("ollama request error")
        || lower.contains("request error")
        || lower.contains("connection refused")
        || lower.contains("connect timeout")
        || lower.contains("dns error")
        || lower.contains("tls handshake")
    {
        "provider_error"
    } else if lower.contains("llm call failed")
        || lower.contains("provider returned")
        || lower.contains("model returned an error")
        || lower.contains("api error")
        || lower.contains(" 4xx")
        || lower.contains(" 5xx")
        || (lower.contains("status") && (lower.contains(" 4") || lower.contains(" 5"))
            && lower.contains("error"))
    {
        "llm_error"
    } else {
        "other"
    }
}

/// Persist a structured `session_failures` row in the background. Logs and
/// swallows any error — failure logging must never break finalize.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_record_failure(
    db: PgPool,
    session_id: Uuid,
    agent_name: String,
    reason: String,
    llm_provider: Option<String>,
    llm_model: Option<String>,
    tracker: &TaskTracker,
) {
    let kind = classify_failure_kind(&reason).to_string();
    tracker.spawn(async move {
        // Best-effort context gathering: pull last tool message + session
        // start-time directly from the DB. None of these are fatal —
        // missing fields just become NULL.
        let last_tool: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT tool_call_id, content FROM messages \
             WHERE session_id = $1 AND role = 'tool' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&db)
        .await
        .ok()
        .flatten();

        // tool_call_id is opaque; resolve a tool_name by matching the
        // matching assistant tool_calls payload. Best effort — None if
        // the timeline lookup fails or the payload is missing.
        let last_tool_name = match &last_tool {
            Some((Some(tcid), _)) => {
                let timeline_match: Option<String> = sqlx::query_scalar(
                    "SELECT payload->>'tool_name' \
                     FROM session_timeline \
                     WHERE session_id = $1 \
                       AND event_type = 'tool_end' \
                       AND payload->>'tool_call_id' = $2 \
                     ORDER BY created_at DESC LIMIT 1",
                )
                .bind(session_id)
                .bind(tcid)
                .fetch_optional(&db)
                .await
                .ok()
                .flatten();
                timeline_match
            }
            _ => None,
        };
        let last_tool_output = last_tool.and_then(|(_, c)| c);

        // Iteration count: count of tool_end events in timeline for this session.
        let iteration_count: Option<i32> = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*)::BIGINT FROM session_timeline \
             WHERE session_id = $1 AND event_type = 'tool_end'",
        )
        .bind(session_id)
        .fetch_one(&db)
        .await
        .ok()
        .and_then(|v| v.map(|n| i32::try_from(n).unwrap_or(i32::MAX)));

        // Duration: NOW() - sessions.started_at.
        let duration_secs: Option<i32> = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT EXTRACT(EPOCH FROM (NOW() - started_at))::DOUBLE PRECISION \
             FROM sessions WHERE id = $1",
        )
        .bind(session_id)
        .fetch_one(&db)
        .await
        .ok()
        .and_then(|v| v.map(|secs| secs.round() as i32));

        let context_json = serde_json::json!({
            "kind": kind,
        });

        let input = NewSessionFailure {
            session_id,
            agent_id: agent_name,
            failure_kind: kind.clone(),
            error_message: reason,
            last_tool_name,
            last_tool_output,
            llm_provider,
            llm_model,
            iteration_count,
            duration_secs,
            context_json: Some(context_json),
        };

        if let Err(e) = record_session_failure(&db, input).await {
            tracing::warn!(
                error = %e,
                session_id = %session_id,
                "failed to record session_failures row"
            );
        }
    });
}

/// Record a failure for the **hard-error path** — i.e. `execute()` or
/// `finalize()` bubbled an `Err` out of an engine entry point (`run.rs`) so the
/// normal `finalize::finalize` failure machinery never ran.
///
/// Without this, the only record of the failure is the `SessionLifecycleGuard`
/// `Drop` fallback, which synthesizes an opaque `guard_dropped` /
/// "guard dropped (early exit)" row with all-NULL diagnostics (no provider,
/// model, tool, or iteration data) — leaving operators blind to *why* the
/// session died. In production this made every hard-error session look
/// identical regardless of root cause (provider outage, tool crash, corrupt
/// context, …).
///
/// This helper instead:
///   1. marks the guard `fail(reason)` with the REAL error string — which sets
///      `recorded = true`, so the guard's `Drop` does NOT also emit a duplicate
///      `guard_dropped` row; and
///   2. spawns a structured `session_failures` row carrying the actual reason +
///      provider/model, classified via `classify_failure_kind`.
///
/// Idempotent: if the guard has already transitioned out of `Running` (a normal
/// `finalize` done/fail/interrupt already ran), this is a no-op — we must not
/// clobber the real outcome or double-record.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_hard_error_failure(
    guard: &mut SessionLifecycleGuard,
    db: PgPool,
    session_id: Uuid,
    agent_name: String,
    reason: String,
    llm_provider: Option<String>,
    llm_model: Option<String>,
    bg_tasks: &TaskTracker,
) {
    if !matches!(guard.outcome, SessionOutcome::Running) {
        // A normal finalize path already resolved this session (done / failed /
        // interrupted). Nothing to do — the Drop fallback is already suppressed.
        return;
    }
    guard.fail(&reason).await;
    spawn_record_failure(
        db,
        session_id,
        agent_name,
        reason,
        llm_provider,
        llm_model,
        bg_tasks,
    );
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
    /// LLM provider name (e.g. `"openai"`, `"anthropic"`) — captured for
    /// structured failure logging. `None` means the engine couldn't surface it.
    pub llm_provider: Option<String>,
    /// LLM model name (e.g. `"gpt-4o-mini"`) — captured for structured
    /// failure logging.
    pub llm_model: Option<String>,
    /// Compressor state to persist to DB so the next session turn can resume
    /// anti-thrash counters and summary state without restarting from zero.
    pub compressor: crate::agent::compressor::Compressor,
    /// Per-agent skill review config — when Some and enabled, `finalize` spawns
    /// a background session analysis after `Done` outcomes with enough tool calls.
    pub skill_review: Option<crate::config::SkillReviewConfig>,
    /// Pre-generated UUID for the final assistant message row.
    /// Matches the UUID sent in the `MessageStart` SSE event so the frontend's
    /// live buffer ID equals the DB row ID, preventing duplicate messages.
    pub assistant_message_id: Uuid,
    /// Shared clarify manager — `finalize` cancels any pending clarify waiters
    /// for this session so they don't hang until timeout when the turn ends
    /// (Done / Failed / Interrupted all release the waiter).
    pub clarify_manager: Arc<ClarifyManager>,
}

// ── finalize() ────────────────────────────────────────────────────────────────

/// Persist the final (or partial) assistant message, transition the lifecycle
/// guard, and (on `Done`) spawn knowledge extraction in the background.
///
/// Returns the saved assistant text so callers can pass it upstream.
#[tracing::instrument(
    name = "pipeline.finalize",
    skip_all,
    fields(
        session_id = %ctx.session_id,
        agent = %ctx.agent_name,
        outcome = match &outcome {
            FinalizeOutcome::Done { .. } => "done",
            FinalizeOutcome::Failed { .. } => "failed",
            FinalizeOutcome::Interrupted { .. } => "interrupted",
        },
    )
)]
pub async fn finalize<S: EventSink>(
    ctx: FinalizeContext,
    outcome: FinalizeOutcome,
    sink: &mut S,
    lifecycle_guard: &mut SessionLifecycleGuard,
) -> anyhow::Result<String> {
    // Cancel any pending clarify waiters for this session so they don't block
    // until timeout. Applies to all outcomes (Done / Failed / Interrupted).
    let n = ctx.clarify_manager.clear_session(ctx.session_id);
    if n > 0 {
        tracing::debug!(
            session_id = %ctx.session_id,
            cancelled = n,
            "finalize: cancelled pending clarify waiters"
        );
    }

    let sm = SessionManager::new(ctx.db.clone());
    let agent_name_ref = ctx.agent_name.as_str();

    let out = match &outcome {
        FinalizeOutcome::Done { assistant_text, thinking_json } => {
            // Resolve the guard before the DB write (C2): if save_message fails the
            // session must not be marked 'failed' by guard Drop — the LLM already
            // produced its response. Failure to persist is non-fatal for session status.
            lifecycle_guard.done().await;
            // Use the pre-generated UUID (sent as `messageId` in the `MessageStart`
            // SSE event) so the DB row ID matches the live buffer ID in the frontend.
            // This prevents duplicate messages caused by `historyIds.has(m.id)` misses.
            if let Err(e) = crate::db::sessions::save_message_ex_with_id(
                &ctx.db,
                ctx.assistant_message_id,
                ctx.session_id,
                "assistant",
                assistant_text,
                None,               // tool_calls
                None,               // tool_call_id
                Some(agent_name_ref),
                thinking_json.as_ref(),
                ctx.user_message_id,
                None,
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
            if let Some(sr_cfg) = &ctx.skill_review
                && sr_cfg.enabled {
                    spawn_skill_review(
                        ctx.db.clone(),
                        ctx.session_id,
                        ctx.agent_name.clone(),
                        ctx.provider.clone(),
                        sr_cfg.min_tool_calls,
                        false,
                        &ctx.bg_tasks,
                    );
                }
            assistant_text.clone()
        }
        FinalizeOutcome::Failed { partial, reason } => {
            if !partial.is_empty() {
                // Use the pre-allocated UUID (ON CONFLICT DO NOTHING) so the partial
                // assistant message has the same ID the frontend buffered in the live
                // overlay. Nil UUID means MessageStart was never emitted (pre-loop
                // cancel) — fall back to auto-generated ID in that case.
                if ctx.assistant_message_id != uuid::Uuid::nil() {
                    let _ = crate::db::sessions::save_message_ex_with_id(
                        &ctx.db,
                        ctx.assistant_message_id,
                        ctx.session_id,
                        "assistant",
                        partial,
                        None,
                        None,
                        Some(agent_name_ref),
                        None,
                        ctx.user_message_id,
                        None,
                    )
                    .await;
                } else {
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
            }
            lifecycle_guard.fail(reason).await;
            // Structured failure log: persist diagnostic row in the background.
            // Never blocks finalize and never propagates an error.
            spawn_record_failure(
                ctx.db.clone(),
                ctx.session_id,
                ctx.agent_name.clone(),
                reason.clone(),
                ctx.llm_provider.clone(),
                ctx.llm_model.clone(),
                &ctx.bg_tasks,
            );
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::Error(reason.clone())))
                .await;
            // Always close the SSE stream with Finish — the frontend uses it as
            // the only reliable signal of "no more events coming". Without this,
            // a Failed turn (subagent timeout, tool error, ...) leaves the live
            // stream half-open and the UI keeps the loader animation running
            // until reconnect retries are exhausted.
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::Finish {
                    finish_reason: "error".to_string(),
                    continuation: false,
                }))
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
            if let Some(sr_cfg) = &ctx.skill_review
                && sr_cfg.enabled {
                    spawn_skill_review(
                        ctx.db.clone(),
                        ctx.session_id,
                        ctx.agent_name.clone(),
                        ctx.provider.clone(),
                        sr_cfg.min_tool_calls,
                        true, // force=true: Failed sessions bypass tool_count gate
                        &ctx.bg_tasks,
                    );
                }
            partial.clone()
        }
        FinalizeOutcome::Interrupted { partial, reason } => {
            if !partial.is_empty() {
                if ctx.assistant_message_id != uuid::Uuid::nil() {
                    let _ = crate::db::sessions::save_message_ex_with_id(
                        &ctx.db,
                        ctx.assistant_message_id,
                        ctx.session_id,
                        "assistant",
                        partial,
                        None,
                        None,
                        Some(agent_name_ref),
                        None,
                        ctx.user_message_id,
                        None,
                    )
                    .await;
                } else {
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
            }
            lifecycle_guard.interrupt(reason).await;
            // Close SSE stream cleanly. Without Finish the frontend would keep
            // the loader running and trigger reconnect attempts even though the
            // turn is fully finalized in DB.
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::Finish {
                    finish_reason: "interrupted".to_string(),
                    continuation: false,
                }))
                .await;
            if let Some(sr_cfg) = &ctx.skill_review
                && sr_cfg.enabled {
                    spawn_skill_review(
                        ctx.db.clone(),
                        ctx.session_id,
                        ctx.agent_name.clone(),
                        ctx.provider.clone(),
                        sr_cfg.min_tool_calls,
                        true, // force=true: Interrupted sessions bypass tool_count gate
                        &ctx.bg_tasks,
                    );
                }
            partial.clone()
        }
    };

    // Persist compaction state so the next session turn can resume with the
    // correct anti-thrash counters and prior summary text.
    let state_json = ctx.compressor.to_json();
    if let Err(e) = crate::db::compaction::set_compaction_state(
        &ctx.db,
        ctx.session_id,
        state_json,
    )
    .await
    {
        tracing::warn!(
            error = %e,
            session_id = %ctx.session_id,
            "failed to save compaction_state"
        );
    }

    Ok(out)
}

// ── finalize_context_from_engine() ───────────────────────────────────────────

/// Construct a `FinalizeContext` from an `AgentEngine` reference.
pub fn finalize_context_from_engine(
    engine: &crate::agent::engine::AgentEngine,
    session_id: Uuid,
    message_count: usize,
    user_message_id: Option<Uuid>,
    compressor: crate::agent::compressor::Compressor,
    assistant_message_id: Uuid,
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
        llm_provider: Some(engine.cfg().provider.name().to_string()),
        llm_model: Some(engine.current_model()),
        compressor,
        skill_review: engine.cfg().agent.skill_review.clone(),
        assistant_message_id,
        clarify_manager: engine.cfg().clarify_manager.clone(),
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
    if message_count >= 2 {
        tracker.spawn(async move {
            crate::agent::knowledge_extractor::extract_and_save(
                db, session_id, agent_name, provider, memory_store,
            )
            .await;
        });
    }
}

// ── spawn_skill_review() ──────────────────────────────────────────────────────

/// Count tool_end timeline events for a session (best-effort, returns 0 on error).
async fn count_tool_calls(db: &PgPool, session_id: Uuid) -> u32 {
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*)::BIGINT FROM session_timeline \
         WHERE session_id = $1 AND event_type = 'tool_end'",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .ok()
    .flatten()
    .and_then(|n| u32::try_from(n).ok())
    .unwrap_or(0)
}

pub(crate) fn spawn_skill_review(
    db: PgPool,
    session_id: Uuid,
    agent_name: String,
    provider: Arc<dyn LlmProvider>,
    min_tool_calls: u32,
    force: bool,
    tracker: &TaskTracker,
) {
    tracker.spawn(async move {
        let tool_count = count_tool_calls(&db, session_id).await;
        if tool_count < min_tool_calls && !force {
            return;
        }
        crate::skills::evolution::review_session_for_skills(
            &db,
            &provider,
            &agent_name,
            session_id,
            force,
        )
        .await;
    });
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
            _messages: &[opex_types::Message],
            _tools: &[opex_types::ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<opex_types::LlmResponse> {
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
            llm_provider: None,
            llm_model: None,
            compressor: crate::agent::compressor::Compressor::new(128_000),
            skill_review: None,
            assistant_message_id: uuid::Uuid::nil(),
            clarify_manager: Arc::new(crate::agent::clarify_manager::ClarifyManager::new_for_test()),
        }
    }

    #[test]
    fn execute_status_done_maps_to_finalize_done() {
        use crate::agent::pipeline::execute::ExecuteStatus;
        let out = execute_status_to_finalize(ExecuteStatus::Done, "hello".into(), None);
        assert!(matches!(out, FinalizeOutcome::Done { assistant_text, .. } if assistant_text == "hello"));
    }

    #[test]
    fn execute_status_failed_preserves_reason() {
        use crate::agent::pipeline::execute::ExecuteStatus;
        let out = execute_status_to_finalize(ExecuteStatus::Failed("timeout".into()), "partial".into(), None);
        assert!(matches!(out, FinalizeOutcome::Failed { reason, .. } if reason == "timeout"));
    }

    #[test]
    fn execute_status_interrupted_preserves_partial() {
        use crate::agent::pipeline::execute::ExecuteStatus;
        let out = execute_status_to_finalize(ExecuteStatus::Interrupted("user"), "mid-text".into(), None);
        assert!(matches!(out, FinalizeOutcome::Interrupted { partial, .. } if partial == "mid-text"));
    }

    #[test]
    fn classify_failure_kind_matrix() {
        use super::classify_failure_kind;
        assert_eq!(
            classify_failure_kind("guard dropped (early exit)"),
            "guard_dropped"
        );
        assert_eq!(
            classify_failure_kind("agent did not complete within 30s"),
            "sub_agent_timeout"
        );
        assert_eq!(
            classify_failure_kind("loop_detected_max_nudges"),
            "tool_error"
        );
        assert_eq!(
            classify_failure_kind("tool 'foo' failed 3 times consecutively"),
            "tool_error"
        );
        assert_eq!(classify_failure_kind("iteration_limit_reached"), "max_iterations");
        assert_eq!(
            classify_failure_kind("LLM call failed: ollama request error"),
            "provider_error"
        );
        assert_eq!(
            classify_failure_kind("LLM call failed: model returned an error"),
            "llm_error"
        );
        assert_eq!(classify_failure_kind("something weird"), "other");
    }

    #[test]
    fn execute_status_done_preserves_thinking_json() {
        use crate::agent::pipeline::execute::ExecuteStatus;
        let json = serde_json::json!({"thinking": "step by step"});
        let out = execute_status_to_finalize(ExecuteStatus::Done, "answer".into(), Some(json.clone()));
        assert!(matches!(out, FinalizeOutcome::Done { thinking_json: Some(j), .. } if j == json));
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
        // Phase: Failed path MUST also emit Finish so the SSE stream closes
        // cleanly. Frontend uses Finish as the single signal of "no more
        // events coming"; without it the loader animation lingers and a
        // reconnect storm starts.
        assert!(
            sink.events
                .iter()
                .any(|e| matches!(
                    e,
                    PipelineEvent::Stream(StreamEvent::Finish { .. })
                )),
            "Finish event MUST follow Error on Failed path",
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
    async fn hard_error_path_records_real_reason_and_suppresses_guard_dropped(pool: PgPool) {
        let session_id =
            crate::db::sessions::create_new_session(&pool, "test-agent", "test-user", "test-channel")
                .await
                .unwrap();

        let bg = TaskTracker::new();
        let mut guard = SessionLifecycleGuard::new(pool.clone(), session_id)
            .with_agent("test-agent")
            // Give the guard its own tracker so IF the Drop fallback ever fired
            // it would have somewhere to spawn — the test asserts it does NOT.
            .with_tracker(Arc::new(TaskTracker::new()));

        record_hard_error_failure(
            &mut guard,
            pool.clone(),
            session_id,
            "test-agent".into(),
            "pipeline error: provider returned 503".into(),
            Some("ollama".into()),
            Some("kimi-k2.6".into()),
            &bg,
        )
        .await;

        // Guard is now Failed → its Drop must NOT synthesize a duplicate row.
        drop(guard);
        bg.close();
        bg.wait().await;

        let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT failure_kind, error_message, llm_provider, llm_model \
             FROM session_failures WHERE session_id = $1",
        )
        .bind(session_id)
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(
            rows.len(),
            1,
            "exactly one failure row — no duplicate guard_dropped from Drop"
        );
        assert_eq!(
            rows[0].1, "pipeline error: provider returned 503",
            "the REAL error reason is captured, not 'guard dropped (early exit)'"
        );
        assert_ne!(
            rows[0].0, "guard_dropped",
            "failure_kind reflects the real error (llm_error), not the opaque Drop fallback"
        );
        assert_eq!(rows[0].2.as_deref(), Some("ollama"), "provider captured");
        assert_eq!(rows[0].3.as_deref(), Some("kimi-k2.6"), "model captured");

        let run_status: Option<String> =
            sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
                .bind(session_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(run_status.as_deref(), Some("failed"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn hard_error_path_is_noop_when_session_already_finalized(pool: PgPool) {
        let session_id =
            crate::db::sessions::create_new_session(&pool, "test-agent", "test-user", "test-channel")
                .await
                .unwrap();

        let bg = TaskTracker::new();
        let mut guard = SessionLifecycleGuard::new(pool.clone(), session_id).with_agent("test-agent");
        // Normal success path already resolved the session.
        guard.done().await;

        // A late Err bubbling after finalize already ran must not clobber the
        // 'done' outcome nor record a spurious failure row.
        record_hard_error_failure(
            &mut guard,
            pool.clone(),
            session_id,
            "test-agent".into(),
            "late pipeline error".into(),
            None,
            None,
            &bg,
        )
        .await;
        drop(guard);
        bg.close();
        bg.wait().await;

        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_failures WHERE session_id = $1")
                .bind(session_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(n, 0, "no failure row when the session already finalized 'done'");

        let run_status: Option<String> =
            sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
                .bind(session_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(run_status.as_deref(), Some("done"), "outcome preserved");
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
        // Phase: Interrupted path MUST emit Finish (without Error) so the
        // frontend stops streaming UI immediately and doesn't loop trying
        // to resume a session the backend has finalized.
        let finish_count = sink
            .events
            .iter()
            .filter(|e| matches!(e, PipelineEvent::Stream(StreamEvent::Finish { .. })))
            .count();
        assert_eq!(
            finish_count, 1,
            "exactly one Finish event on Interrupted, got {finish_count}"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn update_message_step_id_sets_column(pool: PgPool) {
        // Phase 4: update_message_step_id correctly populates the column
        // for an existing intermediate row.
        let session_id =
            crate::db::sessions::create_new_session(&pool, "test-agent", "test-user", "test-channel")
                .await
                .unwrap();
        let row_id = uuid::Uuid::new_v4();
        crate::db::sessions::save_message_ex_with_id(
            &pool,
            row_id,
            session_id,
            "assistant",
            "intermediate text",
            Some(&serde_json::json!([{"id":"call_1","name":"x","arguments":{}}])),
            None,
            Some("test-agent"),
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // Before update — step_id is NULL
        let before: Option<i32> = sqlx::query_scalar(
            "SELECT step_id FROM messages WHERE id = $1",
        )
        .bind(row_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(before.is_none());

        crate::db::sessions::update_message_step_id(&pool, row_id, 2)
            .await
            .unwrap();

        let after: Option<i32> = sqlx::query_scalar(
            "SELECT step_id FROM messages WHERE id = $1",
        )
        .bind(row_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after, Some(2), "step_id column populated");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn update_message_step_id_silent_on_missing_row(pool: PgPool) {
        // No-op contract: updating a non-existent row returns Ok without
        // raising. This matters because step_id update is detached and may
        // race with delete operations.
        let result =
            crate::db::sessions::update_message_step_id(&pool, uuid::Uuid::new_v4(), 5).await;
        assert!(result.is_ok());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn prepend_message_content_prefixes_existing_row(pool: PgPool) {
        // QUICK-260508-0dj: prepend_message_content writes the prefix at the
        // head of the existing content and preserves the tail verbatim.
        // BackgroundMediaTask::deliver_to_channel relies on this so the UI
        // inline parser sees `__file__:{json}\n[SYSTEM] ... dispatched ...`.
        let session_id =
            crate::db::sessions::create_new_session(&pool, "test-agent", "test-user", "test-channel")
                .await
                .unwrap();
        let row_id = uuid::Uuid::new_v4();
        let original = "[SYSTEM] Image dispatched in background; the user will receive a photo message.";
        crate::db::sessions::save_message_ex_with_id(
            &pool,
            row_id,
            session_id,
            "tool",
            original,
            None,
            Some("call_xyz"),
            Some("test-agent"),
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let prefix = "__file__:{\"url\":\"/uploads/abc.png\",\"mediaType\":\"image/png\"}\n";
        crate::db::sessions::prepend_message_content(&pool, row_id, prefix)
            .await
            .unwrap();

        let after: String =
            sqlx::query_scalar("SELECT content FROM messages WHERE id = $1")
                .bind(row_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            after.starts_with(prefix),
            "prepended content must start with prefix; got: {after}"
        );
        assert!(
            after.ends_with(original),
            "original content must be preserved at tail; got: {after}"
        );
        assert_eq!(after.len(), prefix.len() + original.len());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn prepend_message_content_silent_on_missing_row(pool: PgPool) {
        // No-op contract mirrors update_message_step_id_silent_on_missing_row:
        // the persist insert spawns detached and the prepend may race ahead,
        // so a 0-row UPDATE must not error.
        let result = crate::db::sessions::prepend_message_content(
            &pool,
            uuid::Uuid::new_v4(),
            "__file__:{}\n",
        )
        .await;
        assert!(result.is_ok(), "prepend on missing row must be Ok, got: {result:?}");
    }
}
