//! Session entry, user-message persist, ProcessingGuard, slash-command detection.
//!
//! See docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md §3, §5.

use crate::agent::engine::stream::{ProcessingGuard, ProcessingPhase};
use crate::agent::pipeline::sink::{EventSink, PipelineEvent};
use crate::agent::session_manager::{SessionLifecycleGuard, SessionManager};
use crate::agent::tool_loop::LoopDetector;
use hydeclaw_types::{IncomingMessage, Message, MessageRole};
use uuid::Uuid;

// ── Public types ──────────────────────────────────────────────────────────────

/// Outcome of the bootstrap phase — passed directly to the execute phase.
///
/// `lifecycle_guard` is wrapped in `Option` so the adapter can `.take()` it
/// before forwarding `BootstrapOutcome` to `execute()` (avoids partial-move).
pub struct BootstrapOutcome {
    pub session_id: Uuid,
    /// Raw user text after PII redaction / URL enrichment (TODO: Task 10 inlines enrichment).
    pub enriched_text: String,
    pub messages: Vec<Message>,
    pub tools: Vec<hydeclaw_types::ToolDefinition>,
    pub loop_detector: LoopDetector,
    pub processing_guard: ProcessingGuard,
    /// Option so the adapter can take() it before passing BootstrapOutcome to execute().
    pub lifecycle_guard: Option<SessionLifecycleGuard>,
    /// Non-None when the user message was a slash-command that was already handled.
    pub command_output: Option<String>,
    /// ID of the user message just persisted; used by finalize as parent for the assistant reply.
    pub user_message_id: Uuid,
}

/// Input context for the bootstrap phase.
pub struct BootstrapContext<'a> {
    pub msg: &'a IncomingMessage,
    pub resume_session_id: Option<Uuid>,
    pub force_new_session: bool,
    pub use_history: bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Log a WAL "running" event with a single retry on failure.
pub(crate) async fn log_wal_running_with_retry(sm: &SessionManager, session_id: Uuid) {
    if let Err(e) = sm.log_wal_event(session_id, "running", None).await {
        tracing::warn!(session_id = %session_id, error = %e, "failed to log WAL running event, retrying");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if let Err(e2) = sm.log_wal_event(session_id, "running", None).await {
            tracing::error!(session_id = %session_id, error = %e2, "WAL running event retry also failed");
        }
    }
}

/// Extract the sender agent ID from an inter-agent message.
///
/// Returns `Some("AgentName")` when `user_id` starts with `"agent:"`, `None` otherwise.
fn extract_sender_agent_id(user_id: &str) -> Option<&str> {
    if user_id.starts_with("agent:") {
        Some(user_id.trim_start_matches("agent:"))
    } else {
        None
    }
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Bootstrap a session: build context, mark running, emit first Phase event,
/// persist the user message, arm the loop detector, and detect slash-commands.
pub async fn bootstrap<S: EventSink>(
    engine: &crate::agent::engine::AgentEngine,
    ctx: BootstrapContext<'_>,
    sink: &mut S,
) -> anyhow::Result<BootstrapOutcome> {
    // 1. Build context (session_id + message history + tool definitions)
    let crate::agent::context_builder::ContextSnapshot {
        session_id,
        mut messages,
        tools,
    } = engine
        .build_context(
            ctx.msg,
            ctx.use_history,
            ctx.resume_session_id,
            ctx.force_new_session,
        )
        .await?;

    // 2. Atomically claim the session as 'running'. Allows re-entry from any
    //    status, including 'done', so users can continue completed sessions.
    //    Ok(false) means the session was deleted between build_context and here
    //    (race between UI and a concurrent delete) — bail in that case only.
    let sm = SessionManager::new(engine.cfg().db.clone());
    match crate::db::sessions::claim_session_running(&engine.cfg().db, session_id).await {
        Ok(true) => {}
        Ok(false) => {
            anyhow::bail!("session {} not found; bootstrap aborted", session_id);
        }
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "claim_session_running failed");
        }
    }

    // Clean up any streaming message left by a previous crashed run before loading context.
    match crate::db::sessions::cleanup_session_streaming_messages(&engine.cfg().db, session_id).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(session=%session_id, count=%n, "cleaned orphaned streaming messages"),
        Err(e) => tracing::warn!(session=%session_id, error=%e, "cleanup_session_streaming_messages failed"),
    }

    log_wal_running_with_retry(&sm, session_id).await;

    // 3. Emit first Phase event (silently dropped by SseSink; routed by ChannelStatusSink)
    let _ = sink.emit(PipelineEvent::Phase(ProcessingPhase::Thinking)).await;

    // 4. Lifecycle guard (kept in Option so the adapter can .take() it for finalize)
    //    `with_agent` is required for the Drop-path `session_failures` insert
    //    (NOT NULL column). Without it the Drop fallback still marks the session
    //    `failed` in `sessions` + WAL but skips the structured failure row.
    let lifecycle_guard = Some(
        SessionLifecycleGuard::new(engine.cfg().db.clone(), session_id)
            .with_tracker(engine.state().bg_tasks.clone())
            .with_agent(engine.cfg().agent.name.clone()),
    );

    // 5. ProcessingGuard — broadcasts "typing" via ui_event_tx (independent of sink)
    let start_event = serde_json::json!({
        "type": "agent_processing",
        "agent": engine.cfg().agent.name,
        "session_id": session_id.to_string(),
        "status": "start",
        "channel": ctx.msg.channel,
    });
    // Broadcast the start event — sidebar relies on WS `agent_processing`
    // to refresh the session list (ui/src/lib/queries.ts:387). ProcessingGuard
    // only emits the `end` event via Drop; without this explicit start the UI
    // never learns about a newly started session until it completes.
    // Regression fixed 2026-04-20 (pipeline unification had dropped this).
    if let Some(tx) = &engine.state().ui_event_tx {
        let _ = tx.send(start_event.to_string());
    }
    let processing_guard = ProcessingGuard::new(
        engine.state().ui_event_tx.clone(),
        engine.state().processing_tracker.clone(),
        engine.cfg().agent.name.clone(),
        &start_event,
    );

    // 6. Enrich + persist user message
    //    Calls crate::agent::pipeline::subagent::enrich_message_text which:
    //    - redacts PII (emails/phones) before sending to external LLM,
    //    - transcribes voice attachments via toolgate,
    //    - describes image/document attachments via vision,
    //    - auto-fetches URLs mentioned in the text through SSRF-safe client.
    //    Previously lost during the pipeline refactor; restored 2026-04-20.
    let user_text = ctx.msg.text.clone().unwrap_or_default();
    let toolgate_url = engine
        .cfg()
        .app_config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    let enriched_text = crate::agent::pipeline::subagent::enrich_message_text(
        engine.http_client(),
        &engine.cfg().app_config.gateway.listen,
        &toolgate_url,
        &engine.cfg().agent.language,
        &user_text,
        &ctx.msg.attachments,
    )
    .await;

    let sender_agent_id = extract_sender_agent_id(&ctx.msg.user_id);
    // parent_message_id = leaf_message_id: threads the new user message onto
    // the active conversation path so reload-from-active-path can find it.
    // user_message_id is then used as parent for the assistant reply in finalize.
    //
    // If the UI didn't send leaf_message_id (stale cache / race between reload
    // and fetch-messages), fall back to the session's latest completed message
    // so the new turn stays anchored to a real chain instead of floating as a
    // root orphan. Without this fallback, reload-during-stream would leave the
    // user message invisible in active_path (seen 2026-04-20).
    // When user_message_id is provided (forkAndRegenerate path), the branch user
    // message was already persisted by POST /api/sessions/{id}/fork. Reuse it
    // directly to avoid creating a duplicate message in the same branch.
    let user_message_id: uuid::Uuid = if let Some(existing_id) = ctx.msg.user_message_id {
        existing_id
    } else {
        let parent_message_id = match ctx.msg.leaf_message_id {
            Some(id) => Some(id),
            None => sm.latest_leaf_message_id(session_id).await.unwrap_or(None),
        };
        sm.save_message_ex(
            session_id,
            "user",
            &enriched_text,
            None,
            None,
            sender_agent_id,
            None,
            parent_message_id,
        )
        .await?
    };

    // 7. LoopDetector: warm-up from WAL if session has prior tool history (BUG-026).
    //    Restores error-streak state so a looping agent cannot get a free
    //    break_threshold reset after crash/resume.
    //    DB errors are non-fatal — unwrap_or_default() gives a fresh detector
    //    (same behaviour as before this fix).
    let loop_config = engine.tool_loop_config();
    let wal_events = crate::db::session_wal::load_tool_events(&engine.cfg().db, session_id)
        .await
        .unwrap_or_default();
    let loop_detector = LoopDetector::warm_up_from_wal(&loop_config, &wal_events);
    if !wal_events.is_empty() {
        tracing::debug!(session = %session_id, events = wal_events.len(), "LoopDetector warmed from WAL");
    }

    // 8. Slash-command detection (spec §11.1 — future extension point for richer outputs)
    let command_output = match engine.handle_command(&user_text, ctx.msg).await {
        Some(result) => Some(result?),
        None => None,
    };

    // 9. Push user message into message history for the LLM
    // Feed the enriched text to the LLM so it sees the transcribed voice /
    // attachment descriptions / fetched URL contents that enrich_message_text
    // produced. The DB already persists the same enriched_text (above) so
    // reload-from-active-path reproduces exactly what the LLM saw.
    messages.push(Message {
        role: MessageRole::User,
        content: enriched_text.clone(),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    });

    // Compact the history now that the new user message is appended, matching
    // the pre-refactor order: compact before the first LLM call, re-compact
    // between tool iterations (compact_tool_results lives in pipeline::execute).
    // Without this, context windows silently overflow for long sessions.
    engine
        .compact_messages(&mut messages, Some(&loop_detector))
        .await;

    Ok(BootstrapOutcome {
        session_id,
        enriched_text,
        messages,
        tools,
        loop_detector,
        processing_guard,
        lifecycle_guard,
        command_output,
        user_message_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_sender_agent_id_strips_prefix() {
        assert_eq!(extract_sender_agent_id("agent:Arty"), Some("Arty"));
    }

    #[test]
    fn extract_sender_agent_id_empty_name_returns_empty_str() {
        assert_eq!(extract_sender_agent_id("agent:"), Some(""));
    }

    #[test]
    fn extract_sender_agent_id_returns_none_for_human() {
        assert_eq!(extract_sender_agent_id("human:ui"), None);
    }

    #[test]
    fn extract_sender_agent_id_returns_none_for_bare_string() {
        assert_eq!(extract_sender_agent_id("Arty"), None);
    }
}
