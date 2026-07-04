//! `POST /api/chat` — AI SDK SSE chat endpoint (Vercel AI SDK v3 wire
//! format).
//!
//! Pipeline:
//!   client JSON → `ChatSseRequest`
//!     → @-mention routing → `IncomingMessage`
//!     → engine task spawn (writes `StreamEvent` to a bounded channel)
//!     → coalescer task (merges `TextDelta`s on a 16 ms tick)
//!     → converter task ([`super::sse_converter::run_converter`] —
//!       `StreamEvent` → SSE JSON, buffers everything in `StreamRegistry`
//!       for resume support)
//!     → `Sse<ReceiverStream>` to the client.
//!
//! The engine task is intentionally decoupled from the SSE client lifetime:
//! the converter buffers events to the registry regardless of client state,
//! so reconnects via `/api/chat/{id}/stream` see a complete reply.

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

use super::sse_converter::{ConverterCtx, run_converter};
use super::super::super::OpenAiMessage;
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
    attachments: Vec<opex_types::MediaAttachment>,
}

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
            if let Some(content) = &m.content
                && !content.is_empty() {
                    return Some(content.clone());
                }
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

    // Update agent_name to the target agent (may differ from request if @-mention routed)
    let agent_name = engine.name().to_string();
    tracing::info!(agent_name = %agent_name, mentioned = ?mentioned_agent, "mention routing: final target agent");

    // Send cleaned text to LLM (without @mention prefix — prevents LLM from echoing it)
    let msg = opex_types::IncomingMessage {
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
    let (event_tx, event_rx) =
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
    //     connected — see `send_and_buffer!` in sse_converter.rs.
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
    // Child of the request span (contextual parent), NOT the shared current span
    // itself. Instrumenting this detached engine task with `Span::current()`
    // directly let the HTTP request span close (handler returns the SSE stream
    // while the engine task keeps running) with the task still holding it — the
    // next poll re-entered a freed span id and tracing-subscriber's sharded
    // registry panicked on a worker thread. A child keeps the parent alive for
    // the task's lifetime and still inherits the extracted OTel parent context.
    // See `trace_propagation::spawn_traced` for the same fix + rationale.
    let request_span = tracing::info_span!("sse_engine_turn");
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

    // Converter task: StreamEvent → SSE JSON events (Vercel AI SDK v3 UI format).
    // Body lives in [`super::sse_converter`] — full AUDIT:SSE-01/02/03
    // invariants documented at the top of that module. Use `spawn_traced` so
    // the converter inherits the request's span context — SSE events emitted
    // from this task (errors, finish markers) appear under the same trace as
    // `pipeline.execute`.
    let ctx = ConverterCtx {
        db: infra.db.clone(),
        invite_db,
        registry: bus.stream_registry.clone(),
        sse_tx,
        pipeline_cancel,
        agent_name,
        mentioned_for_invite: mentioned_agent,
        user_text_for_title,
    };
    crate::trace_propagation::spawn_traced(run_converter(ctx, event_rx, engine_handle));

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
