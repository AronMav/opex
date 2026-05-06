//! `POST /api/chat` — AI SDK SSE chat endpoint (Vercel AI SDK v3 wire
//! format).
//!
//! Pipeline:
//!   client JSON → `ChatSseRequest`
//!     → @-mention routing → `IncomingMessage`
//!     → engine task spawn (writes `StreamEvent` to a bounded channel)
//!     → coalescer task (merges `TextDelta`s on a 16 ms tick)
//!     → converter task (this file's `loop {}` — `StreamEvent` → SSE JSON,
//!       buffers everything in `StreamRegistry` for resume support)
//!     → `Sse<ReceiverStream>` to the client.
//!
//! The engine task is intentionally decoupled from the SSE client lifetime:
//! the converter buffers events to the registry regardless of client state,
//! so reconnects via `/api/chat/{id}/stream` see a complete reply.

use std::collections::HashMap;
use std::str::FromStr;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::streaming_db::{
    StreamingMessageGuard, build_tools_json, read_streaming_content, upsert_streaming_append,
};
use super::super::super::{OpenAiMessage, sse_types};
use crate::agent::engine::StreamEvent;
use crate::gateway::clusters::{AgentCore, ChannelBus, InfraServices};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ChatSseRequest {
    messages: Vec<OpenAiMessage>,
    agent: Option<String>,
    session_id: Option<String>,
    /// Chat ID from AI SDK frontend (used as `session_id` alias for resume).
    #[serde(default)]
    id: Option<String>,
    /// Force creation of a new session (UI "New Chat" button).
    #[serde(default)]
    force_new_session: bool,
    /// Last message ID in the active path — used as `parent_message_id` for the new user message.
    #[serde(default)]
    leaf_message_id: Option<uuid::Uuid>,
    /// When set, bootstrap reuses this existing message as the user turn instead
    /// of creating a new one. Sent by forkAndRegenerate after the fork API has
    /// already persisted the branch user message.
    #[serde(default)]
    user_message_id: Option<uuid::Uuid>,
    /// File attachments uploaded via /api/media/upload (images, audio, documents).
    #[serde(default)]
    attachments: Vec<hydeclaw_types::MediaAttachment>,
}

#[allow(unused_assignments)]
pub(crate) async fn api_chat_sse(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    Json(req): Json<ChatSseRequest>,
) -> impl IntoResponse {
    let agent_name = req.agent.clone().unwrap_or_default();
    let engine = if agent_name.is_empty() {
        agents.first_engine().await
    } else {
        agents.get_engine(&agent_name).await
    };

    let engine = match engine {
        Some(e) => e,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "no agent available"})),
            )
                .into_response();
        }
    };

    // Find the LAST user message - support both content (v1) and parts (v3) formats
    let user_text = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| {
            // Try content field first (v1 format)
            if let Some(content) = &m.content
                && !content.is_empty() {
                    return Some(content.clone());
                }
            // Try parts field (AI SDK v3 format)
            if let Some(parts) = &m.parts {
                for part in parts.iter().rev() {
                    if part.part_type == "text"
                        && let Some(text) = &part.text
                            && !text.is_empty() {
                                return Some(text.clone());
                            }
                }
            }
            None
        })
        .unwrap_or_default();

    tracing::info!(
        messages_count = req.messages.len(),
        user_text_len = user_text.len(),
        "Processing chat request"
    );

    if req.messages.is_empty() {
        tracing::error!("Request messages array is empty");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "no messages provided"})),
        )
            .into_response();
    }

    let session_id = req
        .session_id
        .as_deref()
        .or(req.id.as_deref())
        .and_then(|s| uuid::Uuid::from_str(s).ok());
    let force_new_session = req.force_new_session && session_id.is_none();
    let user_text_for_title = user_text.clone();

    // ── @-mention routing ──────────────────────────────────────────
    // If user message contains @AgentName, route to that agent instead.
    let all_agent_names: Vec<String> = {
        let map = agents.map.read().await;
        map.keys().cloned().collect()
    };

    tracing::debug!(user_text = %user_text, agents = ?all_agent_names, "mention routing: checking");
    let (engine, cleaned_text, mentioned_agent) = if let Some(mentioned) =
        crate::agent::mention_parser::parse_first_mention(&user_text, &all_agent_names)
    {
        tracing::info!(mentioned = %mentioned, "mention routing: found @-mention");
        // Resolve the mentioned agent's engine
        let mentioned_engine = agents.get_engine(&mentioned).await;
        match mentioned_engine {
            Some(eng) => {
                let cleaned = crate::agent::mention_parser::strip_mention(&user_text, &mentioned);
                (eng, cleaned, Some(mentioned))
            }
            None => (engine, user_text.clone(), None),
        }
    } else {
        tracing::debug!("mention routing: no @-mention found");
        (engine, user_text.clone(), None)
    };

    let _original_session_agent = agent_name.clone();
    // Update agent_name to the target agent (may differ from request if @-mention routed)
    let agent_name = engine.name().to_string();
    tracing::info!(agent_name = %agent_name, mentioned = ?mentioned_agent, "mention routing: final target agent");

    // Send cleaned text to LLM (without @mention prefix — prevents LLM from echoing it)
    let msg = hydeclaw_types::IncomingMessage {
        user_id: crate::agent::channel_kind::channel::UI.to_string(),
        text: Some(cleaned_text),
        attachments: req.attachments,
        agent_id: engine.name().to_string(),
        channel: crate::agent::channel_kind::channel::UI.to_string(),
        context: serde_json::json!({}),
        timestamp: chrono::Utc::now(),
        formatting_prompt: None,
        tool_policy_override: None,
        leaf_message_id: req.leaf_message_id,
        user_message_id: req.user_message_id,
    };

    // Phase 62 RES-01: bounded engine-side channel + coalescing converter task.
    // Engine writes to raw_tx (bounded 256) via EngineEventSender wrapper which
    // enforces the CONTEXT.md contract (text-delta droppable, others never
    // dropped under normal operation). A coalescer task reads raw_rx, merges
    // TextDelta events on a 16 ms tick, and writes to event_tx (unbounded
    // toward the converter — coalescer is the sole producer and is rate-limited
    // by the 16 ms tick, so unbounded downstream is safe).
    let (raw_tx, raw_rx) = tokio::sync::mpsc::channel::<StreamEvent>(256);
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
    let (sse_tx, sse_rx) =
        tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(1024);

    crate::gateway::sse::spawn_coalescing_converter(
        raw_rx,
        event_tx.clone(),
        infra.metrics.clone(),
        msg.agent_id.clone(),
    );

    let engine_event_tx =
        crate::agent::engine_event_sender::EngineEventSender::new(raw_tx);

    // Engine task: process message and emit StreamEvents.
    //
    // Architectural invariant (Fix 3 — see commit message): the engine task is
    // INTENTIONALLY decoupled from the SSE client lifetime.
    //
    //   * Spawned via `bus.bg_tasks.spawn(...)` (TaskTracker) so graceful
    //     shutdown waits for it to complete normally before the runtime exits.
    //   * Writes events to `raw_tx` → coalescer → `event_tx`. The downstream
    //     converter task ALWAYS buffers events into `StreamRegistry`
    //     (broadcast-backed) regardless of whether the SSE client is still
    //     connected — see the `send_and_buffer!` macro below.
    //   * If the SSE client disconnects, the converter sets
    //     `client_gone_since` but keeps draining engine events to the registry
    //     so reconnects via `/api/chat/{id}/stream` see a complete reply.
    //   * The only paths that abort the engine task externally are:
    //       (a) the 30s grace window after an explicit user-initiated cancel
    //           (POST /api/chat/{id}/abort), and
    //       (b) the 600s runaway-protection window after the SSE client is
    //           confirmed gone with no resume.
    //   Neither path runs unless the converter task observes a deliberate
    //   signal — converter panics, channel hiccups, or transient client
    //   drops do NOT cancel the engine.
    //
    // Inter-agent communication happens via the `agent` tool (polling model),
    // so no turn loop is needed — a single handle_sse call suffices.
    let ui_tx = bus.ui_event_tx.clone();
    let agent_for_broadcast = msg.agent_id.clone();
    let invite_db = infra.db.clone();
    let mentioned_for_invite = mentioned_agent.clone();
    // C3: create the pipeline cancel token here, before spawning, so it can be
    // shared between handle_sse (pipeline execution) and StreamRegistry (abort API).
    // All clones share cancellation state: POST /abort → registry.cancel() →
    // pipeline_cancel.cancel() → engine_cancel propagates into execute().
    let pipeline_cancel = CancellationToken::new();
    let engine_cancel = pipeline_cancel.clone();
    // Capture the current tracing span (which includes the OTel parent
    // context extracted by `trace_propagation::extract_trace_context_layer`)
    // and bind the spawned future to it via `.instrument()`. Without this,
    // `tokio::spawn` would start the future under an empty span context and
    // child spans inside `pipeline::execute` would land in a fresh trace,
    // disconnected from any upstream `traceparent` we honoured at the
    // gateway boundary.
    use tracing::Instrument as _;
    let request_span = tracing::Span::current();
    let engine_handle = bus.bg_tasks.spawn(async move {
        let current_agent_name = engine.name().to_string();
        if let Err(e) = engine.handle_sse(&msg, engine_event_tx.clone(), session_id, force_new_session, engine_cancel).await {
            tracing::error!(error = %e, "SSE chat error (agent: {})", current_agent_name);
            // Error is a non-text event — use send_async to honor CONTEXT.md
            // "non-text never dropped" contract. send_async awaits a slot on
            // the bounded channel and only errors if the channel is closed.
            let _ = engine_event_tx.send_async(StreamEvent::Error(e.to_string())).await;
        }

        // Notify UI about session update so sidebar refreshes
        let event = serde_json::json!({
            "type": "session_updated",
            "agent": agent_for_broadcast,
            "channel": crate::agent::channel_kind::channel::UI,
        });
        ui_tx.send(event.to_string()).ok();
    }.instrument(request_span));

    // Converter task: StreamEvent → SSE JSON events (Vercel AI SDK v3 UI format)
    // Based on @ai-sdk/react UIMessageChunk types
    // Also buffers events in StreamRegistry for resume support.
    //
    // AUDIT:SSE-01 (verified 2026-03-30): Event ordering is guaranteed by single-task
    // sequential processing. The `while let Some(event) = event_rx.recv().await` loop
    // is the sole consumer of engine events. `current_text_id` accumulates all
    // consecutive TextDelta events under ONE text-start..text-end block; text-end is
    // flushed before any non-text event (Finish, Error, ToolCallStart). Without this,
    // each TextDelta would emit its own start/end → N text parts on the UI for one
    // logical text block, and adjacent parts could fuse word boundaries on render.
    //
    // AUDIT:SSE-02 (verified 2026-03-30): Error delivery has two paths:
    // 1. LLM errors mid-stream: engine sends error as TextDelta via format_user_error()
    //    (intentional -- user sees error inline in chat history), then Finish event.
    // 2. handle_sse() top-level errors: chat.rs sends StreamEvent::Error via event_tx,
    //    converter sends error SSE event, marks stream as error in registry, finalizes
    //    the streaming message, then sends [DONE]. Client always receives error before
    //    connection close in both paths.
    let registry = bus.stream_registry.clone();
    // Use `spawn_traced` so the converter inherits the request's
    // span context. SSE events emitted from this task (errors, finish
    // markers) appear under the same trace as `pipeline.execute`.
    crate::trace_propagation::spawn_traced(async move {
        let mut text_id_counter: usize = 0;
        // Tracks the OPEN text block so consecutive TextDelta events all carry the
        // same id. None = no open block; Some(id) = block id `id` is currently open.
        let mut current_text_id: Option<String> = None;
        let mut tool_name_map: HashMap<String, String> = HashMap::new();
        let mut session_id_str: Option<String> = None;
        // Tracks which agent is currently responding (updated on AgentSwitch)
        let mut current_responding_agent = agent_name.clone();
        tracing::debug!(current_responding_agent = %current_responding_agent, "converter: initial agent for SSE");
        #[allow(unused_assignments)]
        let mut client_gone_since: Option<std::time::Instant> = None;

        // Helper: send SSE event to client (if connected) and always buffer in registry
        macro_rules! send_and_buffer {
            ($json_str:expr) => {{
                let s: &str = &$json_str;
                tracing::debug!(target: "SSE-OUT", agent = %agent_name, sid = ?session_id_str, event = %&s[..s.len().min(180)], "emit");
                let seq: u64 = if let Some(ref sid) = session_id_str {
                    registry.push_event(sid, &$json_str).await
                } else {
                    0
                };
                if !sse_tx.is_closed() {
                    client_gone_since = None;
                    // SSE `id:` field — client tracks via Last-Event-ID for
                    // dedup-free reconnect. seq=0 (no session yet) emits no id.
                    let event = if seq > 0 {
                        Event::default().id(seq.to_string()).data($json_str)
                    } else {
                        Event::default().data($json_str)
                    };
                    sse_tx.try_send(Ok(event)).is_ok()
                } else {
                    // Client disconnected — keep buffering for DB save + resume.
                    // Do NOT abort the engine: let it finish naturally so the result
                    // is saved to DB and the frontend picks it up via polling on reload.
                    // Engine has its own limits (max_iterations, subagent timeout).
                    if client_gone_since.is_none() {
                        client_gone_since = Some(std::time::Instant::now());
                        tracing::info!("SSE client disconnected, continuing engine for DB save");
                    }
                    true // always keep going — abort only via cancel API
                }
            }};
        }

        let mut finished_sent = false;
        let mut cancel_token: Option<CancellationToken> = None;
        let mut job_id: Option<uuid::Uuid> = None;
        let chat_db = infra.db.clone();
        let mut accumulated_text = String::new();
        let mut accumulated_tools: Vec<serde_json::Value> = Vec::new();
        let mut tools_flushed_count: usize = 0;
        let mut cached_tools_json: Option<serde_json::Value> = None;
        // Periodic DB flush for streaming messages (LibreChat-style)
        let mut streaming_msg_id = uuid::Uuid::new_v4();
        let mut streaming_guard = StreamingMessageGuard::new(infra.db.clone(), streaming_msg_id);
        let mut last_db_flush = std::time::Instant::now();
        let mut session_uuid: Option<uuid::Uuid> = None;
        let flush_interval = std::time::Duration::from_secs(2);
        // On explicit API cancel (POST /api/chat/{id}/abort) we do NOT
        // hard-abort `engine_handle` immediately. The CancellationToken
        // cascades through providers' `stream_with_cancellation` and raises
        // `LlmCallError::UserCancelled` with partial_state; the engine's error
        // path then persists the aborted message row and writes an aborted
        // usage_log entry. We give it a bounded window (CANCEL_GRACE) to
        // finish naturally, then hard-abort if it's wedged. This guards
        // against tool loops or sync blocks that ignore the cancel token
        // (code_exec, workspace_write, std::sync::Mutex contention) — the
        // pre-existing `client_gone_since > 600 s` check only ran inside
        // `event_rx.recv().await`, so a silent wedge bypassed it.
        const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
        let mut logged_cancel_drain = false;
        let mut cancel_deadline: Option<tokio::time::Instant> = None;
        loop {
            // Record the cancel-observation + deadline once.
            if cancel_token
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                && !logged_cancel_drain
            {
                logged_cancel_drain = true;
                cancel_deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
                tracing::info!(
                    session_id = ?session_id_str,
                    "user cancel received; engine has {}s to emit aborted message + Finish",
                    CANCEL_GRACE.as_secs(),
                );
            }

            // Race the engine's next event against the cancel-grace
            // deadline (if set). The helper encapsulates the three
            // outcomes so the logic can be unit-tested in isolation
            // via `tokio::time::pause()`.
            use super::super::cancel_grace::{
                GracePollResult, poll_event_with_cancel_grace,
            };
            let event = match poll_event_with_cancel_grace(&mut event_rx, cancel_deadline).await
            {
                GracePollResult::Event(ev) => ev,
                GracePollResult::Closed => break, // engine finished / channel closed
                GracePollResult::GraceExceeded => {
                    // Grace window exceeded with no event progress — the
                    // engine is wedged ignoring the cancel token. Force
                    // an abort so the task / semaphore permit are freed.
                    //
                    // This branch fires ONLY after a user-initiated cancel
                    // (cancel_deadline is set above only when cancel_token
                    // is cancelled). Mark the session `'interrupted'`
                    // BEFORE hard-abort so the UI shows "INTERRUPTED"
                    // instead of "ERROR" — the engine task's
                    // `SessionLifecycleGuard` is about to drop, and its
                    // Drop impl would otherwise mark the session `'failed'`.
                    // The guard uses `mark_session_run_status_if_running`,
                    // so once we write `'interrupted'` here the guard's
                    // update affects 0 rows.
                    tracing::warn!(
                        session_id = ?session_id_str,
                        "cancel grace window ({}s) exceeded, hard-aborting engine",
                        CANCEL_GRACE.as_secs(),
                    );
                    if let Some(sid) = session_uuid
                        && let Err(e) = crate::db::sessions::mark_session_run_status_if_running(
                            &infra.db,
                            sid,
                            "interrupted",
                        )
                        .await
                    {
                        tracing::warn!(
                            session_id = %sid,
                            error = %e,
                            "failed to mark session interrupted before hard-abort"
                        );
                    }
                    engine_handle.abort();
                    break;
                }
            };

            // AUDIT:SSE-03 (verified 2026-03-30): Safety net for client disconnect.
            // See stream_registry.rs for full SSE-03 audit. This 10-minute timeout
            // ensures no hanging tasks if client disconnects and never reconnects.
            // Safety net: abort if client gone for 10+ minutes (runaway engine protection)
            if client_gone_since.is_some_and(|t| t.elapsed().as_secs() > 600) {
                tracing::warn!("SSE client gone for 10min, aborting runaway engine");
                engine_handle.abort();
                break;
            }
            // Close the open text block before any non-text event. Consecutive
            // TextDelta events keep the block open and share the same id.
            if !matches!(event, StreamEvent::TextDelta(_))
                && let Some(text_id) = current_text_id.take() {
                let end_data = json!({"type": sse_types::TEXT_END, "id": text_id}).to_string();
                let _ = send_and_buffer!(end_data);
            }

            let data = match event {
                StreamEvent::SessionId { session_id: sid, context_limit } => {
                    let context_limit = context_limit;
                    let parsed_uuid = uuid::Uuid::from_str(&sid).ok();
                    // Register stream in registry for resume + abort support (C3).
                    // Use register_with_token so the registry stores the SAME token
                    // that was passed to handle_sse: POST /api/chat/{id}/abort →
                    // registry.cancel() cancels pipeline_cancel → propagates into execute().
                    if let Some(uuid) = parsed_uuid
                        && let Some(jid) = registry.register_with_token(uuid, &agent_name, pipeline_cancel.clone()).await {
                            cancel_token = Some(pipeline_cancel.clone());
                            job_id = Some(jid);
                        }
                    session_id_str = Some(sid.clone());
                    session_uuid = parsed_uuid;
                    if let Some(sid_uuid) = session_uuid {
                        streaming_guard.set_session_id(sid_uuid);
                    }
                    // Auto-invite the mentioned agent if it differs from the session owner
                    if let Some(sid_uuid) = session_uuid
                        && let Some(ref mentioned) = mentioned_for_invite
                    {
                        let db = invite_db.clone();
                        let agent = mentioned.clone();
                        tokio::spawn(async move {
                            let _ = crate::db::sessions::add_participant(&db, sid_uuid, &agent).await;
                        });
                    }
                    // Write empty streaming record immediately — gives frontend a persistent DB signal
                    // before the first token arrives. Single source of truth for "is agent thinking?".
                    if let Some(sid_uuid) = session_uuid
                        && let Err(e) = crate::db::sessions::upsert_streaming_message(
                            &chat_db, streaming_msg_id, sid_uuid, &agent_name, "", None
                        ).await {
                            tracing::warn!(error = %e, "failed to upsert initial streaming message to DB");
                        }
                    // Custom data part: session_id for UI to track the active session
                    json!({"type": sse_types::DATA_SESSION_ID, "data": {"sessionId": sid, "contextLimit": context_limit}, "transient": true})
                }
                StreamEvent::MessageStart { message_id } => {
                    json!({"type": sse_types::START, "messageId": message_id, "agentName": current_responding_agent})
                }
                StreamEvent::StepStart { step_id, message_id } => {
                    // Boundary between LLM tool-loop iterations. `messageId`
                    // is the pre-allocated DB row UUID for the iteration —
                    // frontend opens a fresh live ChatMessage with this id so
                    // it matches the eventual DB row. Open text block was
                    // already closed by the non-TextDelta guard at the top
                    // of the loop.
                    json!({"type": sse_types::STEP_START, "stepId": step_id, "messageId": message_id, "agentName": current_responding_agent})
                }
                StreamEvent::TextDelta(ref text) => {
                    if session_uuid.is_none() && accumulated_text.is_empty() {
                        tracing::error!("TextDelta received but session_uuid is None — DB flush will be skipped");
                    }
                    // AI SDK v3: text-start → text-delta* → text-end
                    // Open a new text block only if there isn't one open already; all
                    // consecutive deltas of the same logical text block share one id.
                    let text_id = match current_text_id.as_ref() {
                        Some(id) => id.clone(),
                        None => {
                            text_id_counter += 1;
                            let new_id = format!("text-{text_id_counter}");
                            let start_data = json!({"type": sse_types::TEXT_START, "id": new_id.clone(), "agentName": current_responding_agent}).to_string();
                            let _ = send_and_buffer!(start_data);
                            current_text_id = Some(new_id.clone());
                            new_id
                        }
                    };
                    let delta_data = json!({"type": sse_types::TEXT_DELTA, "id": text_id, "delta": text}).to_string();
                    let _ = send_and_buffer!(delta_data);
                    accumulated_text.push_str(text);
                    // Periodic DB flush every 2s so reload shows partial response
                    // Uses append-mode SQL so accumulated_text can be cleared after flush (bounded memory)
                    if last_db_flush.elapsed() >= flush_interval
                        && let Some(sid) = session_uuid {
                            let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
                            if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                                tracing::warn!(error = %e, "failed to flush streaming message to DB");
                            } else {
                                // Only clear after successful flush -- on failure, text stays for retry
                                accumulated_text.clear();
                            }
                            last_db_flush = std::time::Instant::now();
                        }
                    continue;
                }
                StreamEvent::ToolCallStart { id, name } => {
                    tool_name_map.insert(id.clone(), name.clone());
                    json!({
                        "type": sse_types::TOOL_INPUT_START,
                        "toolCallId": id,
                        "toolName": name,
                        "agentName": current_responding_agent,
                    })
                }
                StreamEvent::ToolCallArgs { id, args_text } => {
                    let delta_data = json!({
                        "type": sse_types::TOOL_INPUT_DELTA,
                        "toolCallId": id,
                        "inputTextDelta": args_text
                    }).to_string();
                    let _ = send_and_buffer!(delta_data);

                    let input: serde_json::Value = serde_json::from_str(&args_text)
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    let tool_name = tool_name_map.get(&id).cloned().unwrap_or_default();
                    json!({
                        "type": sse_types::TOOL_INPUT_AVAILABLE,
                        "toolCallId": id,
                        "toolName": tool_name,
                        "input": input
                    })
                }
                StreamEvent::ToolResult { ref id, ref result } => {
                    // Accumulate tool calls in-memory (single DB write at finish)
                    let tname = tool_name_map.get(id).cloned().unwrap_or_default();
                    accumulated_tools.push(json!({"toolCallId": id, "toolName": tname, "output": result}));
                    cached_tools_json = None; // Invalidate cache when new tool arrives
                    json!({
                        "type": sse_types::TOOL_OUTPUT_AVAILABLE,
                        "toolCallId": id,
                        "output": result
                    })
                }
                StreamEvent::StepFinish { step_id: _, finish_reason: _ } => {
                    continue;
                }
                StreamEvent::RichCard { card_type, data } => {
                    json!({
                        "type": sse_types::RICH_CARD,
                        "cardType": card_type,
                        "data": data
                    })
                }
                StreamEvent::File { url, media_type } => {
                    json!({
                        "type": sse_types::FILE,
                        "url": url,
                        "mediaType": media_type
                    })
                }
                // Retained for API compatibility — not currently emitted.
                StreamEvent::AgentSwitch { agent_name: new_agent } => {
                    current_responding_agent = new_agent;
                    continue; // Internal event — don't emit SSE
                }
                StreamEvent::ApprovalNeeded { approval_id, tool_name, tool_input, timeout_ms } => {
                    let data = json!({
                        "type": sse_types::APPROVAL_NEEDED,
                        "approvalId": approval_id,
                        "toolName": tool_name,
                        "toolInput": tool_input,
                        "timeoutMs": timeout_ms,
                    }).to_string();
                    let _ = send_and_buffer!(data);
                    continue;
                }
                StreamEvent::ApprovalResolved { approval_id, action, modified_input } => {
                    let data = json!({
                        "type": sse_types::APPROVAL_RESOLVED,
                        "approvalId": approval_id,
                        "action": action,
                        "modifiedInput": modified_input,
                    }).to_string();
                    let _ = send_and_buffer!(data);
                    continue;
                }
                StreamEvent::Finish { .. } => {
                    // Close any still-open text block before Finish.
                    if let Some(text_id) = current_text_id.take() {
                        let end_data = json!({"type": sse_types::TEXT_END, "id": text_id}).to_string();
                        let _ = send_and_buffer!(end_data);
                    }
                    let finish_data = json!({"type": sse_types::FINISH, "agentName": current_responding_agent}).to_string();
                    let _ = send_and_buffer!(finish_data);
                    // Final flush of streaming message + mark complete
                    // CRITICAL ORDERING: upsert → read_streaming_content → set_content → finalize (DELETE)
                    if let Some(sid) = session_uuid {
                        let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
                        // Step 1: Flush remaining text delta to streaming message (APPEND mode)
                        if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                            tracing::warn!(error = %e, "failed to upsert streaming message on Finish");
                        }
                        // Step 2: Read back full aggregated text BEFORE the row is deleted
                        let full_text = read_streaming_content(&chat_db, streaming_msg_id).await;
                        // Step 3: Persist full content to stream_jobs (needs complete text)
                        if let Some(jid) = job_id
                            && let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, jid, &full_text, &accumulated_tools).await {
                                tracing::warn!(error = %e, "failed to set stream job content on Finish");
                            }
                        // Step 4: NOW safe to finalize (DELETE) the streaming message row
                        if let Err(e) = crate::db::sessions::finalize_streaming_message(&chat_db, streaming_msg_id).await {
                            tracing::warn!(error = %e, "failed to finalize streaming message on Finish");
                        }
                    }
                    streaming_guard.mark_finalized();
                    // DON'T break here — session-scoped agents may send more events.
                    // The loop exits naturally when event_tx is dropped (engine task completes).
                    // Send [DONE] only after all turns are done (handled in post-loop block).
                    // Reset accumulated state for next agent turn:
                    accumulated_text.clear();
                    accumulated_tools.clear();
                    tools_flushed_count = 0;
                    cached_tools_json = None;
                    streaming_msg_id = uuid::Uuid::new_v4();
                    text_id_counter = 0;
                    // Reset guard for next turn to prevent streaming row leak
                    streaming_guard = StreamingMessageGuard::new(chat_db.clone(), streaming_msg_id);
                    if let Some(sid) = session_uuid {
                        streaming_guard.set_session_id(sid);
                    }
                    continue;
                }
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                    reasoning_tokens,
                } => {
                    // agentName: tag with the currently-responding agent so the UI
                    // can route this usage to the correct agent's tokenUsage state.
                    // Without it, in a multi-agent session a usage event from agent
                    // B fired while A is mid-stream would overwrite A's state.
                    let mut payload = serde_json::json!({
                        "type": sse_types::USAGE,
                        "inputTokens": input_tokens,
                        "outputTokens": output_tokens,
                        "agentName": current_responding_agent.clone(),
                    });
                    // Extended fields — subsets of input/output (not additive). Only emit
                    // when present so older clients see no change in payload size for
                    // providers that don't report them.
                    if let Some(v) = cache_read_tokens {
                        payload["cacheReadTokens"] = serde_json::Value::from(v);
                    }
                    if let Some(v) = cache_creation_tokens {
                        payload["cacheCreationTokens"] = serde_json::Value::from(v);
                    }
                    if let Some(v) = reasoning_tokens {
                        payload["reasoningTokens"] = serde_json::Value::from(v);
                    }
                    let data = payload.to_string();
                    let _ = send_and_buffer!(data);
                    continue;
                }
                StreamEvent::Reconnecting { attempt, delay_ms } => {
                    let data = serde_json::json!({
                        "type": sse_types::RECONNECTING,
                        "attempt": attempt,
                        "delay_ms": delay_ms,
                    }).to_string();
                    let _ = send_and_buffer!(data);
                    continue;
                }
                StreamEvent::Error(ref text) => {
                    let err_data = json!({"type": sse_types::ERROR, "errorText": text}).to_string();
                    let _ = send_and_buffer!(err_data);
                    if let Some(ref sid) = session_id_str {
                        registry.mark_error(sid, text).await;
                    }
                    // Finalize streaming message on error too
                    // CRITICAL ORDERING: upsert → read_streaming_content → set_content → finalize (DELETE)
                    if let Some(sid) = session_uuid {
                        let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
                        // Step 1: Flush remaining text delta (APPEND mode)
                        if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                            tracing::warn!(error = %e, "failed to upsert streaming message on Error");
                        }
                        // Step 2: Read back full aggregated text BEFORE the row is deleted
                        let full_text = read_streaming_content(&chat_db, streaming_msg_id).await;
                        // Step 3: Persist full content to stream_jobs
                        if let Some(jid) = job_id
                            && let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, jid, &full_text, &accumulated_tools).await {
                                tracing::warn!(error = %e, "failed to set stream job content on Error");
                            }
                        // Step 4: NOW safe to finalize (DELETE) the streaming message row
                        if let Err(e) = crate::db::sessions::finalize_streaming_message(&chat_db, streaming_msg_id).await {
                            tracing::warn!(error = %e, "failed to finalize streaming message on Error");
                        }
                    }
                    streaming_guard.mark_finalized();
                    finished_sent = true;
                    break;
                }
            };

            let json_str = data.to_string();
            let _ = send_and_buffer!(json_str);
        }

        // Only send [DONE] and mark_finished if the Finish branch didn't already do it
        if !finished_sent {
            // Finalize streaming message on unexpected exit
            // CRITICAL ORDERING: upsert → read_streaming_content → set_content → finalize (DELETE)
            if let Some(sid) = session_uuid {
                let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
                // Step 1: Flush remaining text delta (APPEND mode)
                if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                    tracing::warn!(error = %e, "failed to upsert streaming message on unexpected exit");
                }
                // Step 2: Read back full aggregated text BEFORE the row is deleted
                let full_text = read_streaming_content(&chat_db, streaming_msg_id).await;
                // Step 3: Persist full content to stream_jobs
                if let Some(jid) = job_id
                    && let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, jid, &full_text, &accumulated_tools).await {
                        tracing::warn!(error = %e, "failed to set stream job content on unexpected exit");
                    }
                // Step 4: NOW safe to finalize (DELETE) the streaming message row
                if let Err(e) = crate::db::sessions::finalize_streaming_message(&chat_db, streaming_msg_id).await {
                    tracing::warn!(error = %e, "failed to finalize streaming message on unexpected exit");
                }
            }
            streaming_guard.mark_finalized();
            if let Some(ref sid) = session_id_str {
                registry.mark_finished(sid).await;
            }
            // Flush any remaining text-end (if stream ended without Finish event)
            if let Some(text_id) = current_text_id {
                let end_data = json!({"type": sse_types::TEXT_END, "id": text_id});
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    sse_tx.send(Ok(Event::default().data(end_data.to_string())))
                ).await;
            }
            // [DONE] is critical — use timeout to avoid blocking if client gone
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                sse_tx.send(Ok(Event::default().data("[DONE]")))
            ).await;
        }

        // Auto-title: set session title from first user message if not already titled
        if let Some(sid) = session_uuid {
            let title_db = chat_db.clone();
            // spawn_traced so the auto-title query appears under the
            // originating request's trace (lets us see how long the
            // auto-title query took relative to the rest of the turn).
            crate::trace_propagation::spawn_traced(async move {
                if let Err(e) = crate::db::sessions::auto_title_session(&title_db, sid, &user_text_for_title).await {
                    tracing::debug!(error = %e, "auto-title failed");
                }
            });
        }

        // Session agent pool is NOT cleaned up here — agents live until
        // explicitly killed via agent(action: "kill") or session expiry.
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);

    (
        [(
            axum::http::header::HeaderName::from_static("x-vercel-ai-ui-message-stream"),
            "v1",
        )],
        Sse::new(stream).keep_alive(KeepAlive::default()),
    )
        .into_response()
}
