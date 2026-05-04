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
    /// Opaque context echoed from the incoming message (e.g. `{"chat_id": 123}` for Telegram).
    /// Empty `Value::Null` for UI / internal callers. Threaded through to channel_actions
    /// so YAML tools with `channel_action: send_voice` can deliver media to the originating chat.
    pub incoming_context: serde_json::Value,
    /// Channel name (e.g. "telegram", "ui"); empty string when unavailable.
    pub channel: String,
    /// Proactive compression state loaded from DB (or fresh if no prior session state).
    pub compressor: crate::agent::compressor::Compressor,
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

// ── Chain-split helper ────────────────────────────────────────────────────────

/// Check if `session_id` has a pending chain split.
/// If `pending_split=true` in the session's compaction_state:
///   1. Creates child session B in DB
///   2. Inserts compressed seed messages (system + summary + tail) into B
///   3. Marks A with end_reason='compression'
///   4. Saves updated compaction_state (pending_split=false) on B
///   5. Returns Ok(Some(child_id))
///
/// On any DB error after child creation: logs warn, continues — fail-open.
/// Returns Ok(None) when no split is needed or on pre-creation error.
async fn maybe_split_session(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    preserve_last_n: usize,
) -> anyhow::Result<Option<uuid::Uuid>> {
    // Load compaction_state
    let state_json = match crate::db::compaction::get_compaction_state(db, session_id).await? {
        Some(s) => s,
        None => return Ok(None),
    };

    // Deserialize — check pending_split flag
    let mut state: crate::agent::compressor::CompressorState =
        match serde_json::from_value(state_json) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, session = %session_id, "cannot parse compaction_state for split check");
                return Ok(None);
            }
        };

    if !state.pending_split {
        return Ok(None);
    }

    // Load session metadata needed for child creation
    let (agent_id, user_id, channel, title) =
        match crate::db::sessions::get_session_for_chain(db, session_id).await? {
            Some(row) => row,
            None => {
                tracing::warn!(session = %session_id, "session not found for chain split");
                return Ok(None);
            }
        };

    // Load system message (if present) — used as head of seed
    let system_msg = sqlx::query_as::<_, (String,)>(
        "SELECT content FROM messages
         WHERE session_id = $1 AND role = 'system'
         ORDER BY created_at ASC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None)
    .map(|(content,)| hydeclaw_types::Message {
        role: hydeclaw_types::MessageRole::System,
        content,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    });

    // Load all non-system messages in chronological order, then take last preserve_last_n
    let all_rows = sqlx::query_as::<_, (String, String, Option<serde_json::Value>, Option<String>)>(
        "SELECT role, content, tool_calls, tool_call_id
         FROM messages
         WHERE session_id = $1 AND role != 'system'
         ORDER BY created_at ASC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let tail: Vec<hydeclaw_types::Message> = all_rows
        .into_iter()
        .rev()
        .take(preserve_last_n)
        .rev()  // restore chronological order
        .map(|(role, content, tool_calls, tool_call_id)| {
            let msg_role = match role.as_str() {
                "assistant" => hydeclaw_types::MessageRole::Assistant,
                "tool"      => hydeclaw_types::MessageRole::Tool,
                _           => hydeclaw_types::MessageRole::User,
            };
            hydeclaw_types::Message {
                role: msg_role,
                content,
                tool_calls: tool_calls.and_then(|v| serde_json::from_value(v).ok()),
                tool_call_id,
                thinking_blocks: vec![],
            }
        })
        .collect();

    let summary = state.previous_summary.as_deref().unwrap_or("");
    let seed = crate::agent::history::build_compressed_seed(system_msg.as_ref(), summary, &tail);

    // Create child session — fail-open if this errors
    let child_id = match crate::db::sessions::create_chain_session(
        db,
        session_id,
        &agent_id,
        &user_id,
        &channel,
        title.as_deref(),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                error = %e,
                session = %session_id,
                "create_chain_session failed — continuing in parent"
            );
            return Ok(None);
        }
    };

    // Insert seed messages into child — if this fails the child is empty and
    // useless; fall back to parent session rather than redirecting into a void.
    if let Err(e) = crate::db::sessions::insert_seed_messages(db, child_id, &agent_id, &seed).await {
        tracing::warn!(error = %e, child = %child_id, "insert_seed_messages failed — continuing in parent session");
        // Clean up the empty child session to prevent orphaned sessions in the DB.
        if let Err(del_err) = sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(child_id)
            .execute(db)
            .await
        {
            tracing::warn!(error = %del_err, child = %child_id, "failed to delete orphaned child session");
        }
        return Ok(None);
    }

    // Mark parent as ended
    if let Err(e) = crate::db::sessions::set_session_end_reason(db, session_id, "compression").await {
        tracing::warn!(error = %e, session = %session_id, "set_session_end_reason failed");
    }

    // Clear pending_split on parent — prevents re-entering split on future resumes of the parent.
    state.pending_split = false;
    let parent_cleared = serde_json::to_value(&state).unwrap_or(serde_json::Value::Null);
    if let Err(e) = crate::db::compaction::set_compaction_state(db, session_id, parent_cleared).await {
        tracing::warn!(error = %e, session = %session_id, "could not clear pending_split on parent");
    }

    // Save compaction_state on child: inherit only previous_summary (the compression seed),
    // but reset counters — child is a fresh continuation that should compress freely.
    let child_state = crate::agent::compressor::CompressorState {
        previous_summary: state.previous_summary.clone(),
        ineffective_count: 0,
        compression_count: 0,
        pending_split: false,
    };
    let new_state_json = serde_json::to_value(&child_state).unwrap_or(serde_json::Value::Null);
    if let Err(e) = crate::db::compaction::set_compaction_state(db, child_id, new_state_json).await {
        tracing::warn!(error = %e, child = %child_id, "set_compaction_state on child failed");
    }

    tracing::info!(
        parent = %session_id,
        child = %child_id,
        tail_count = tail.len(),
        "compression chain split complete"
    );
    Ok(Some(child_id))
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Bootstrap a session: build context, mark running, emit first Phase event,
/// persist the user message, arm the loop detector, and detect slash-commands.
pub async fn bootstrap<S: EventSink>(
    engine: &crate::agent::engine::AgentEngine,
    ctx: BootstrapContext<'_>,
    sink: &mut S,
) -> anyhow::Result<BootstrapOutcome> {
    // Compute context_limit once for Compressor construction (used on all paths below).
    let context_limit = crate::agent::pipeline::llm_call::default_context_for_model(
        &engine.cfg().agent.model,
    ) as u32;

    // Pre-build chain split: if the resume target has pending_split=true in its
    // compaction_state, create a child session and redirect there. This must happen
    // before build_context so all guards, messages, and the session WAL use the
    // child's session_id from the start.
    let effective_resume_id: Option<uuid::Uuid> = if !ctx.force_new_session {
        if let Some(resume_id) = ctx.resume_session_id {
            let preserve_last_n = engine
                .cfg()
                .agent
                .compaction
                .as_ref()
                .map_or(10, |c| c.preserve_last_n as usize);
            match maybe_split_session(&engine.cfg().db, resume_id, preserve_last_n).await {
                Ok(Some(child_id)) => Some(child_id),
                Ok(None) => Some(resume_id),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        session = %resume_id,
                        "maybe_split_session error — continuing in original session"
                    );
                    Some(resume_id)
                }
            }
        } else {
            None
        }
    } else {
        ctx.resume_session_id
    };

    // 1. Build context (session_id + message history + tool definitions)
    let crate::agent::context_builder::ContextSnapshot {
        session_id,
        mut messages,
        tools,
    } = engine
        .build_context(
            ctx.msg,
            ctx.use_history,
            effective_resume_id,   // ← was ctx.resume_session_id
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

    // Clean up any streaming message left by a previous crashed run.
    // Runs after build_context; startup cleanup_interrupted_sessions() handles the immediate
    // post-crash case, this call handles re-entry within the same process lifetime.
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

    // Load compaction state from DB so proactive compression in execute() can
    // resume where the previous session turn left off (anti-thrash counters, summary).
    let compaction_state =
        crate::db::compaction::get_compaction_state(&engine.cfg().db, session_id)
            .await
            .unwrap_or(None);
    let compressor =
        crate::agent::compressor::Compressor::load(compaction_state, context_limit);

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
        incoming_context: ctx.msg.context.clone(),
        channel: ctx.msg.channel.clone(),
        compressor,
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
