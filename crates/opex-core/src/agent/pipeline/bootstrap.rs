//! Session entry, user-message persist, ProcessingGuard, slash-command detection.
//!
//! See docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md §3, §5.

use crate::agent::commands::spec::CommandOutcome;
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
    pub command_output: Option<CommandOutcome>,
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
    /// Wave-2 Task 12: one-shot per-turn model override, sourced from
    /// `POST /api/chat`'s `ChatSseRequest.model`. `bootstrap()` itself never
    /// sets this (always `None` here) — `bootstrap_sse` (the only caller that
    /// can receive a per-turn override from a client request) stamps it onto
    /// the returned `BootstrapOutcome` afterward. Threaded by `pipeline::execute`
    /// into every iteration's `CallOptions.model_override`. NEVER written to
    /// `provider.set_model_override()` — scoped to this turn only, never
    /// leaks into a concurrent or subsequent turn on the shared engine.
    pub turn_model_override: Option<String>,
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

    // Persist-clean true orphan tool-result rows for THIS session on entry (a
    // crash can commit a tool-result while losing its parent assistant). The
    // read-path filter already keeps them out of context; this stops them
    // accumulating without waiting for the next process restart's global sweep.
    // Boundary-split results (assistant present but excluded by compaction) are
    // NOT touched — they are valid history, not orphans.
    match crate::db::sessions::sweep_orphan_tool_results_for_session(&engine.cfg().db, session_id).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(session=%session_id, count=%n, "swept orphaned tool-result rows on session entry"),
        Err(e) => tracing::warn!(session=%session_id, error=%e, "session orphan-tool sweep failed (non-fatal)"),
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
    let start_ws_event = opex_types::ws::WsEvent::AgentProcessing {
        agent: engine.cfg().agent.name.clone(),
        status: "start".to_string(),
        session_id: Some(session_id.to_string()),
        channel: Some(ctx.msg.channel.clone()),
    };
    let start_event = serde_json::to_value(&start_ws_event).unwrap_or_default();
    // Broadcast the start event — sidebar relies on WS `agent_processing`
    // to refresh the session list (ui/src/lib/queries.ts:387). ProcessingGuard
    // only emits the `end` event via Drop; without this explicit start the UI
    // never learns about a newly started session until it completes.
    // Regression fixed 2026-04-20 (pipeline unification had dropped this).
    if let Some(tx) = &engine.state().ui_event_tx {
        let _ = tx.send(start_ws_event.to_json());
    }
    let processing_guard = ProcessingGuard::new(
        engine.state().ui_event_tx.clone(),
        engine.state().processing_tracker.clone(),
        engine.cfg().agent.name.clone(),
        &start_event,
    );

    // 6. Persist user message BEFORE enrichment.
    //
    // Architectural fix: enrichment (URL fetch / voice transcription / vision)
    // can take seconds. The previous order did enrichment FIRST and only then
    // INSERTed the user row, which meant a refresh during enrichment saw an
    // orphan session in the DB (no user message) and the UI rendered an empty
    // chat. With client-side session_id pre-allocation + early user_message
    // persist, a refresh during enrichment now finds both the session AND the
    // user message — the UI can render the user's text immediately and
    // auto-resume the live stream once the engine task starts emitting.
    //
    // The ORIGINAL text is persisted (enrichment output is LLM context only —
    // it would pollute the UI with fetched URL contents / PII-redacted marks).
    // The enriched text is pushed into the in-memory `messages` vec below for
    // the LLM call only.
    let user_text = ctx.msg.text.clone().unwrap_or_default();
    let sender_agent_id = extract_sender_agent_id(&ctx.msg.user_id);
    // parent_message_id = leaf_message_id: threads the new user message onto
    // the active conversation path so reload-from-active-path can find it.
    //
    // Server-authoritative resolution: the client's leaf_message_id is a HINT,
    // not gospel. It can dangle — an optimistic client UUID that never reached
    // the DB, a prior turn that failed to persist (e.g. a tool row with a NUL
    // byte broke the chain and the session went `interrupted`), or a branch
    // race. A dangling id threaded straight into the `parent_message_id` FK
    // would make THIS INSERT fail (`messages_parent_message_id_fkey`) and crash
    // the whole send. So we VALIDATE it exists in this session; if it doesn't
    // (or the UI sent none), fall back to the real persisted leaf, else NULL
    // (root). The new turn always anchors to a row that actually exists.
    let parent_message_id = match ctx.msg.leaf_message_id {
        Some(id) if sm.message_exists_in_session(id, session_id).await.unwrap_or(false) => Some(id),
        _ => sm.latest_leaf_message_id(session_id).await.unwrap_or(None),
    };
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
            None, // user rows carry no tool-loop step index
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

    // 7. Enrich (slow) — runs AFTER the user message is durable so a refresh
    //    during enrichment still finds the conversation in a consistent state.
    //    Hard 60s outer timeout: each internal step (URL fetch 10s, vision via
    //    toolgate, voice transcription) has its own timeout, but a hung
    //    toolgate can still chain past those — this is the safety net so
    //    bootstrap can never block the engine task indefinitely on enrichment.
    let toolgate_url = engine
        .cfg()
        .app_config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    const ENRICHMENT_HARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
    tracing::info!(
        attachment_count = ctx.msg.attachments.len(),
        user_text_len = user_text.len(),
        "bootstrap: starting enrichment"
    );
    let enrich = match tokio::time::timeout(
        ENRICHMENT_HARD_TIMEOUT,
        crate::agent::pipeline::subagent::enrich_message_text(
            engine.http_client(),
            &engine.cfg().app_config.gateway.listen,
            &toolgate_url,
            &user_text,
            &ctx.msg.attachments,
            &engine.cfg().handler_registry,
            &engine.cfg().db,
            &engine.cfg().agent.language,
        ),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => {
            tracing::warn!(
                session_id = %session_id,
                "enrichment exceeded 60s hard timeout; proceeding with original text"
            );
            crate::agent::pipeline::subagent::EnrichResult { text: user_text.clone() }
        }
    };
    let enriched_text = enrich.text;

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
            match crate::db::session_timeline::load_tool_events(&engine.cfg().db,
                session_id,
            ).await
            {
                Ok(events) => events,
                Err(e) => {
                    tracing::warn!(
                        session = %session_id,
                        error = %e,
                        "failed to load timeline tool events; LoopDetector warm-up skipped"
                    );
                    vec![]
                }
            };
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
        Some(result) => match result {
            Ok(out) => Some(out),
            Err(e) => {
                // H8 fix: persist a synthetic assistant error message before
                // bubbling — otherwise the user message is orphaned in the DB
                // (no reply row) and the lifecycle-guard Drop marks the
                // session `interrupted` with an opaque diagnostic. A visible
                // assistant row that explains the failure gives the user a
                // clear signal that the command (not the engine) errored.
                let reason = format!("Command failed: {e}");
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "slash-command handler returned an error; persisting synthetic assistant message"
                );
                let error_text = format!("⚠️ {reason}");
                let _ = sm
                    .save_message_ex(
                        session_id,
                        "assistant",
                        &error_text,
                        None,
                        None,
                        Some(&engine.cfg().agent.name),
                        None,
                        Some(user_message_id),
                    )
                    .await;
                return Err(e);
            }
        },
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

    // G3 (WS5): pre-turn history compaction is NO LONGER done here. It made a
    // blocking LLM call on the SYNCHRONOUS POST path (bootstrap_sse runs before
    // the 202 is returned), so a slow/dead compaction provider stalled the turn
    // into the UI's 30s client timeout. It now runs at the top of the detached
    // engine turn (`pipeline::execute`, before the first LLM call) where a
    // budget-exceeding compaction is invisible to the send-POST. The compaction
    // itself is budgeted + fail-open (see `history::COMPACTION_BUDGET`). This
    // relocation preserves cron/isolated behaviour: those callers route through
    // the SAME `pipeline::execute` and so still compact before their first call.

    // Load compaction state from DB so proactive compression in execute() can
    // resume where the previous session turn left off (anti-thrash counters, summary).
    let compaction_state =
        match crate::db::compaction::get_compaction_state(&engine.cfg().db,
            session_id,
        ).await
        {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(
                    session = %session_id,
                    error = %e,
                    "failed to load compaction state; starting fresh"
                );
                None
            }
        };
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
        turn_model_override: None,
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

