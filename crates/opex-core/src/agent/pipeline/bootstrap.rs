//! Session entry, user-message persist, ProcessingGuard, slash-command detection.
//!
//! See docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md §3, §5.

use crate::agent::engine::stream::{ProcessingGuard, ProcessingPhase};
use crate::agent::pipeline::sink::{EventSink, PipelineEvent};
use crate::agent::session_manager::{SessionLifecycleGuard, SessionManager};
use crate::agent::tool_loop::LoopDetector;
use opex_types::{IncomingMessage, Message, MessageRole};
use uuid::Uuid;

// ── Public types ──────────────────────────────────────────────────────────────

/// Outcome of the bootstrap phase — passed directly to the execute phase.
///
/// `lifecycle_guard` is wrapped in `Option` so the adapter can `.take()` it
/// before forwarding `BootstrapOutcome` to `execute()` (avoids partial-move).
pub struct BootstrapOutcome {
    pub session_id: Uuid,
    /// Raw user text after PII redaction / URL enrichment.
    pub enriched_text: String,
    pub messages: Vec<Message>,
    pub tools: Vec<opex_types::ToolDefinition>,
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
    /// CACHE-02: per-agent CLAUDE.md content for the third cache
    /// breakpoint. Forwarded from `ContextSnapshot.claude_md_content`.
    /// `None` for non-base agents and agents without prompt_cache.
    pub claude_md_content: Option<String>,
    /// `true` when an async-video job was accepted during enrich (YouTube link
    /// enqueued, or a `summarize_video` attachment outcome). The SSE/channel
    /// adapters short-circuit the LLM loop and persist `video_ack_text` as the
    /// assistant reply instead of running the agent.
    pub video_accepted: bool,
    /// Clean user-facing acknowledgement to emit as the assistant reply when
    /// `video_accepted` is `true` (empty otherwise). NOT the enriched blob.
    pub video_ack_text: String,
}

/// Input context for the bootstrap phase.
pub struct BootstrapContext<'a> {
    pub msg: &'a IncomingMessage,
    pub resume_session_id: Option<Uuid>,
    pub force_new_session: bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Log a timeline "running" event with a single retry on failure.
pub(crate) async fn log_timeline_running_with_retry(sm: &SessionManager, session_id: Uuid) {
    if let Err(e) = sm.log_timeline_event(session_id, "running", None).await {
        tracing::warn!(session_id = %session_id, error = %e, "failed to log timeline running event, retrying");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if let Err(e2) = sm.log_timeline_event(session_id, "running", None).await {
            tracing::error!(session_id = %session_id, error = %e2, "timeline running event retry also failed");
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
    // Compute context_limit once for Compressor construction (used on all paths below).
    // For Ollama providers this queries /api/show and caches the result in-process.
    // Resolve by the EFFECTIVE model (current_model), not the static config
    // model, so a runtime model override (m073) resolves the override's window.
    let context_limit = crate::agent::pipeline::llm_call::resolve_context_limit(
        engine.cfg().provider.as_ref(),
        &engine.current_model(),
    ).await;

    let effective_resume_id: Option<uuid::Uuid> = ctx.resume_session_id;

    // 1. Build context (session_id + message history + tool definitions)
    //
    // The second positional argument is `include_tools` in `ContextBuilder::build`.
    // Historically `ctx.use_history` was forwarded here, but the two concepts are
    // orthogonal — history is loaded separately in `build()` and never gated on
    // this flag. Wiring `use_history` here was a regression from the LLM-loop
    // unification (f3356ada): cron / streaming callers passed `use_history:
    // false` and silently lost all tools, which caused LLMs to hallucinate XML
    // tool-calls (e.g. `<sequentialthinking>...</sequentialthinking>`) that the
    // OpenAI-compatible provider then persists as plain assistant text.
    //
    // Always request tools — every entry point (SSE, channel, streaming, cron)
    // needs them.
    let crate::agent::context_builder::ContextSnapshot {
        session_id,
        mut messages,
        tools,
        reentry_mode,
        claude_md_content,
        breakdown,
    } = engine
        .build_context(
            ctx.msg,
            true,
            effective_resume_id,   // ← was ctx.resume_session_id
            ctx.force_new_session,
        )
        .await?;

    // T17: cache the estimate-only breakdown for GET /api/agents/{name}/context-breakdown.
    // Best-effort — never blocks or fails bootstrap.
    engine.state().set_context_breakdown(breakdown).await;
    // 2. Atomically claim the session as 'running' using mode-aware semantics.
    //    `claim_session_with_retry` retries once with `ExplicitResume` if a
    //    narrow TOCTOU race (status flipped between resolve and claim) caused
    //    the strict per-mode WHERE guard to miss. `Ok(false)` after retry
    //    means the row was deleted — bail.
    let sm = SessionManager::new(engine.cfg().db.clone());
    match crate::db::sessions::claim_session_with_retry(
        &engine.cfg().db,
        session_id,
        reentry_mode,
    ).await {
        Ok(true) => {}
        Ok(false) => {
            anyhow::bail!("session {} not claimable after retry; bootstrap aborted", session_id);
        }
        Err(e) => {
            // Must propagate: continuing without a successful claim allows two
            // concurrent SSE handlers to race on the same session (both write
            // timeline "running", both persist assistant messages).
            tracing::error!(session_id = %session_id, error = %e, "claim_session_with_retry failed; aborting bootstrap");
            return Err(e);
        }
    }

    // Граница нового хода: prune старья (best-effort, не блокирует ход).
    if let Some(cm) = engine.cfg().checkpoint_manager.as_ref()
        && let Err(e) = cm.new_turn(&engine.cfg().agent.name).await
    {
        tracing::warn!(error = %e, "checkpoint new_turn failed (non-fatal)");
    }

    // Persist the originating channel chat_id (if any) so an interactive `/goal`
    // interrupted by a restart can be channel-pushed on the next boot, not only
    // surfaced in the UI bell. Best-effort: web/UI sessions carry no chat_id, and
    // a failed stamp must never break the turn.
    if let Some(chat_id) = ctx.msg.context.get("chat_id").and_then(|v| v.as_i64())
        && let Err(e) = crate::db::sessions::set_session_chat_id(&engine.cfg().db, session_id, chat_id).await
    {
        tracing::warn!(session_id = %session_id, error = %e, "failed to stamp session chat_id (non-fatal)");
    }

    // Clean up any streaming message left by a previous crashed run.
    // Runs after build_context; startup cleanup_interrupted_sessions() handles the immediate
    // post-crash case, this call handles re-entry within the same process lifetime.
    match crate::db::sessions::cleanup_session_streaming_messages(&engine.cfg().db, session_id).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(session=%session_id, count=%n, "cleaned orphaned streaming messages"),
        Err(e) => tracing::warn!(session=%session_id, error=%e, "cleanup_session_streaming_messages failed"),
    }

    log_timeline_running_with_retry(&sm, session_id).await;

    // 3. Emit first Phase event (silently dropped by SseSink; routed by ChannelStatusSink)
    let _ = sink.emit(PipelineEvent::Phase(ProcessingPhase::Thinking)).await;

    // 4. Lifecycle guard (kept in Option so the adapter can .take() it for finalize)
    //    `with_agent` is required for the Drop-path `session_failures` insert
    //    (NOT NULL column). Without it the Drop fallback still marks the session
    //    `failed` in `sessions` + timeline but skips the structured failure row.
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
    let enrich = crate::agent::pipeline::subagent::enrich_message_text(
        engine.http_client(),
        &engine.cfg().app_config.gateway.listen,
        &toolgate_url,
        &user_text,
        &ctx.msg.attachments,
    )
    .await;
    let enriched_text = enrich.text;
    let video_accepted = enrich.video_accepted;
    // Clean user-facing ack for the short-circuit reply (never the whole enriched
    // blob, which carries PII-redacted text). The async-video accept always comes
    // from a detected video link now, so the ack is the canonical constant.
    let video_ack_text = if video_accepted {
        "🎬 Видео по ссылке принято, готовлю сводку.".to_string()
    } else {
        String::new()
    };

    // Decision-webhooks for BeforeMessage: block the turn or inject context.
    let bm_event = crate::agent::hooks::HookEvent::BeforeMessage;
    let bm_decision = engine.hooks().fire_decision(
        &bm_event,
        serde_json::json!({ "message": enriched_text }),
    ).await;
    let enriched_text = match bm_decision {
        crate::agent::hooks::HookDecision::Block(reason) => {
            engine.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                agent_name: engine.cfg().agent.name.clone(),
                session_id: Some(session_id),
                event_type: "BeforeMessage".into(),
                action: "Block".into(),
                detail: Some(reason.chars().take(512).collect()),
            });
            anyhow::bail!("blocked by hook: {}", reason);
        }
        crate::agent::hooks::HookDecision::InjectContext(ctx_inject) => {
            engine.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                agent_name: engine.cfg().agent.name.clone(),
                session_id: Some(session_id),
                event_type: "BeforeMessage".into(),
                action: "InjectContext".into(),
                detail: None,
            });
            // Inject hook context ahead of the user message (provenance already tagged).
            format!("{ctx_inject}\n\n{enriched_text}")
        }
        _ => enriched_text,
    };

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
    // When user_message_id is provided the client pre-allocated the UUID so the
    // optimistic message in the live overlay has the same ID as the DB row
    // (symmetric to the assistant message pre-allocation in execute.rs). We save
    // with save_message_ex_with_id (ON CONFLICT DO NOTHING) so the fork path is
    // safe: if the message was already inserted by POST /fork, the insert is a
    // no-op and we reuse the same UUID — no duplicate, no error.
    let parent_message_id = match ctx.msg.leaf_message_id {
        Some(id) => Some(id),
        None => sm.latest_leaf_message_id(session_id).await.unwrap_or(None),
    };
    // Store the original user text in DB — enriched_text is LLM context only
    // and must not be persisted (URL fetched content would show in the UI).
    let user_message_id: uuid::Uuid = if let Some(prealloc_id) = ctx.msg.user_message_id {
        crate::db::sessions::save_message_ex_with_id(
            &engine.cfg().db,
            prealloc_id,
            session_id,
            "user",
            &user_text,
            None,
            None,
            sender_agent_id,
            None,
            parent_message_id,
            None,
        )
        .await?;
        prealloc_id
    } else {
        sm.save_message_ex(
            session_id,
            "user",
            &user_text,
            None,
            None,
            sender_agent_id,
            None,
            parent_message_id,
        )
        .await?
    };

    // 7. LoopDetector: warm from timeline ONLY when this is a true continuation
    //    of an in-flight run (`ResumeRunning` after a crash, or `ExplicitResume`
    //    where the user re-opened a session via UI). For `NewSession` and
    //    `NewTurnAfterDone` the user is starting a fresh conversational turn —
    //    prior tool errors from a previous turn are not relevant and would
    //    falsely trip `error_break_threshold`. (BUG-026 originally added the
    //    warm-up unconditionally; we now scope it to crash-recovery only.)
    let loop_config = engine.tool_loop_config();
    let loop_detector = if reentry_mode.warm_loop_detector() {
        let timeline_events =
            crate::db::session_timeline::load_tool_events(&engine.cfg().db, session_id)
                .await
                .unwrap_or_default();
        if !timeline_events.is_empty() {
            tracing::debug!(
                session = %session_id,
                events = timeline_events.len(),
                ?reentry_mode,
                "LoopDetector warmed from timeline",
            );
        }
        LoopDetector::warm_up_from_timeline(&loop_config, &timeline_events)
    } else {
        LoopDetector::new(&loop_config)
    };

    // 8. Slash-command detection (spec §11.1 — future extension point for richer outputs)
    let command_output = match engine.handle_command(&user_text, ctx.msg).await {
        Some(result) => Some(result?),
        None => None,
    };

    // 9. Push user message into message history for the LLM
    // Feed the enriched text to the LLM so it sees the transcribed voice /
    // attachment descriptions / fetched URL contents that enrich_message_text
    // produced. DB stores only user_text (original); enriched_text is LLM-only.
    messages.push(Message {
        role: MessageRole::User,
        content: enriched_text.clone(),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
            db_id: None,
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
        claude_md_content,
        video_accepted,
        video_ack_text,
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

