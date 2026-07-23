//! `POST /api/chat` — server-authoritative chat entry (T3). Returns `202
//! {session_id, user_message_id}` after a SYNCHRONOUS bootstrap + stream
//! registration; it no longer streams the reply itself.
//!
//! Pipeline:
//!   client JSON → `ChatSseRequest`
//!     → @-mention routing → `IncomingMessage`
//!     → `engine.bootstrap_sse` (synchronous: resolves session_id +
//!       user_message_id = the stream boundary)
//!     → `StreamRegistry::register_with_token` (BEFORE responding, so a later
//!       GET /{id}/stream is guaranteed to find the stream)
//!     → engine task spawn (`engine.execute_sse`, writes `StreamEvent`s)
//!     → coalescer task (merges `TextDelta`s on a 16 ms tick)
//!     → converter task ([`super::sse_converter::run_converter`] —
//!       `StreamEvent` → SSE JSON, buffers everything in `StreamRegistry`
//!       for resume) → `202` returned.
//!
//! The engine task is intentionally decoupled from any client lifetime: the
//! converter buffers events to the registry regardless of client state, and the
//! reply is streamed by `GET /api/chat/{id}/stream` (T4) which replays the
//! registry buffer.

use std::str::FromStr;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::Event,
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
    /// Wave-2 Task 12: one-shot per-turn model override — applies to every
    /// LLM call in THIS turn only, via `CallOptions.model_override`. Never
    /// mutates the agent's configured model or the shared
    /// `provider.set_model_override()` state, so it cannot leak into a
    /// concurrent or subsequent turn. Absence (or an empty/whitespace-only
    /// string, normalized to `None` in `api_chat_sse`) preserves prior
    /// behaviour exactly.
    #[serde(default)]
    model: Option<String>,
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
    // UI pre-allocation: when the client sends `session_id` together with
    // `force_new_session=true` it has already saved that UUID to localStorage
    // (so a refresh during POST can still find the session). Honour both: keep
    // force_new_session set, and thread the client id through msg.context so
    // the context builder uses `create_new_session_with_id` instead of
    // generating a fresh UUID server-side. Previously the `&& session_id.is_none()`
    // guard silently dropped the client id and forced a server-generated one,
    // which defeated the pre-allocation.
    let client_session_id_for_new = if req.force_new_session { session_id } else { None };
    let force_new_session = req.force_new_session;
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
    let mut msg = opex_types::IncomingMessage {
        user_id: crate::agent::channel_kind::channel::UI.to_string(),
        text: Some(cleaned_text),
        attachments: req.attachments,
        agent_id: engine.name().to_string(),
        channel: crate::agent::channel_kind::channel::UI.to_string(),
        // Thread the client-provided session id (if any) through to the context
        // builder so a force_new_session branch can honour the pre-allocated UUID.
        context: if let Some(cid) = client_session_id_for_new {
            serde_json::json!({ "client_session_id": cid.to_string() })
        } else {
            serde_json::json!({})
        },
        timestamp: chrono::Utc::now(),
        formatting_prompt: None,
        tool_policy_override: None,
        leaf_message_id: req.leaf_message_id,
        user_message_id: req.user_message_id,
    };

    // ── T3: server-authoritative stream. POST runs bootstrap SYNCHRONOUSLY,
    // registers the stream, and returns 202 {session_id, user_message_id}; the
    // engine + converter run detached, buffering into StreamRegistry. A later
    // GET /api/chat/{id}/stream (T4) replays that buffer. ─────────────────────

    // C3: create the pipeline cancel token here so it is shared between
    // execute_sse (pipeline execution) and StreamRegistry (abort API). All
    // clones share cancellation state: POST /abort → registry.cancel() →
    // pipeline_cancel.cancel() → engine_cancel propagates into execute().
    let pipeline_cancel = CancellationToken::new();

    // 1. Synchronous bootstrap resolves session_id + user_message_id (= the
    //    stream boundary). NOTE — honest latency: user_message_id only exists
    //    AFTER `enrich_message_text` (voice transcription / vision / URL fetch),
    //    so a POST carrying attachments or URLs can take seconds before this
    //    returns 202. Accepted by design: the UI's optimistic echo covers the
    //    pause; reordering bootstrap is out of scope.
    //    G3 (WS5): history compaction is NO LONGER on this synchronous path — it
    //    moved to the detached `pipeline::execute` (before the first LLM call),
    //    so a slow/dead compaction provider can't stall the 202.
    // Wave-2 Task 12: normalize the per-turn model override at the wire
    // boundary — trim + empty string ⇒ None, so a client sending `"model": ""`
    // (or all-whitespace) behaves identically to omitting the field.
    let model_override = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // When `force_new_session=true`, do NOT pass the (possibly client-provided)
    // session_id as `resume_session_id` — context_builder's first branch would
    // try to resume it (and fail because the session doesn't exist yet). The
    // client-provided id is threaded via `msg.context.client_session_id` and
    // context_builder's `force_new_session` branch honours it via
    // `session_create_new_with_id`. The resume path stays unchanged for the
    // existing-session case (force_new_session=false + session_id=Some).
    let resume_session_id = if force_new_session { None } else { session_id };

    // Determine the session_id + user_message_id UP-FRONT so we can register
    // the stream and return 202 BEFORE bootstrap runs. Bootstrap+execute move
    // into the detached engine task — a client disconnect (page refresh during
    // POST) no longer drops the bootstrap future mid-way, leaving an orphan
    // session row with run_status=NULL and no user_message (the exact bug we
    // just fixed).
    //
    //   * New chat (force_new=true): use the client-provided id if present,
    //     otherwise generate fresh. The id is stamped onto msg.context so
    //     context_builder uses `session_create_new_with_id` and the row gets
    //     the EXACT id we promised in the 202.
    //   * Existing chat (session_id=Some, not force_new): use that id directly.
    //   * Legacy (neither): we don't know the id up-front, so fall through to
    //     the synchronous bootstrap path below. Channels don't hit this path
    //     (they use handle_with_status / handle_streamming), so the cost is
    //     limited to misbehaving UI clients.
    let preallocated_session_id = if force_new_session {
        let sid = session_id.unwrap_or_else(uuid::Uuid::new_v4);
        // Ensure context_builder's force_new branch picks up THIS id (whether
        // client-provided or freshly generated). Without this, bootstrap would
        // generate its own UUID for the no-pre-allocation case and the 202
        // we return would point at a non-existent session.
        msg.context = serde_json::json!({ "client_session_id": sid.to_string() });
        Some(sid)
    } else if session_id.is_some() {
        session_id
    } else {
        None
    };

    let preallocated_user_message_id = req.user_message_id.or_else(|| {
        // For the preallocated path we need a user_message_id too — generate
        // one and stamp it on the msg so bootstrap's save_message_ex_with_id
        // uses it. The id is also echoed in the 202 below.
        if preallocated_session_id.is_some() {
            let id = uuid::Uuid::new_v4();
            msg.user_message_id = Some(id);
            Some(id)
        } else {
            None
        }
    });

    // Preallocated path: create session row + register stream + spawn detached engine task + 202.
    if let Some(resp_session_id) = preallocated_session_id {
        let resp_user_message_id = match preallocated_user_message_id {
            Some(id) => id,
            None => {
                tracing::error!("preallocated session_id without user_message_id — client invariant violated");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "preallocated session_id without user_message_id"})),
                )
                    .into_response();
            }
        };

        // Create the session row BEFORE register_with_token — the stream_jobs
        // table has a FK on session_id, so an INSERT there for a not-yet-created
        // session violates the constraint and register_with_token returns None
        // (which the user sees as "stream registry at capacity"). For force_new
        // we use the pre-allocated id; for resume (session_id=Some, not
        // force_new) the session already exists and we skip this step.
        if force_new_session
            && let Err(e) = opex_db::sessions::create_new_session_with_id(
                &infra.db,
                resp_session_id,
                &agent_name,
                crate::agent::channel_kind::channel::UI,
                crate::agent::channel_kind::channel::UI,
            )
            .await
        {
            tracing::error!(error = %e, "preallocated session create failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }

        // Persist the user message BEFORE the engine task spawns. Otherwise an
        // F5 between spawn and bootstrap reaching save_message_ex_with_id
        // (bootstrap does ~10 setup steps before persist) leaves the session
        // with no user row — the user's message vanishes on reload. With
        // pre-allocated user_message_id + ON CONFLICT DO NOTHING in
        // save_message_ex_with_id, the later bootstrap persist becomes a no-op
        // (same UUID) so we don't duplicate. parent_message_id follows the
        // same rule bootstrap uses (msg.leaf_message_id, else session's latest
        // leaf — None for a fresh session).
        let parent_message_id = match msg.leaf_message_id {
            Some(id) => Some(id),
            None => {
                let sm = crate::agent::session_manager::SessionManager::new(infra.db.clone());
                sm.latest_leaf_message_id(resp_session_id).await.unwrap_or(None)
            }
        };
        if let Err(e) = opex_db::sessions::save_message_ex_with_id(
            &infra.db,
            resp_user_message_id,
            resp_session_id,
            "user",
            &user_text,
            None,
            None,
            None,
            None,
            parent_message_id,
            None,
            None,
        )
        .await
        {
            tracing::error!(error = %e, "preallocated user_message persist failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }

        // Register the stream BEFORE responding so a subsequent GET /{id}/stream
        // finds it. The engine task will populate the buffer.
        let Some(job_id) = bus
            .stream_registry
            .register_with_token(
                resp_session_id,
                &agent_name,
                pipeline_cancel.clone(),
                resp_user_message_id,
            )
            .await
        else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "stream registry at capacity"})),
            )
                .into_response();
        };

        // Phase 62 RES-01: bounded engine-side channel + coalescing converter task.
        let (raw_tx, raw_rx) = tokio::sync::mpsc::channel::<StreamEvent>(256);
        let (event_tx, event_rx) =
            tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        let (sse_tx, sse_rx) =
            tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(1024);
        drop(sse_rx);

        crate::gateway::sse::spawn_coalescing_converter(
            raw_rx,
            event_tx.clone(),
            infra.metrics.clone(),
            msg.agent_id.clone(),
        );

        let engine_event_tx = crate::agent::engine_event_sender::EngineEventSender::new(raw_tx);

        // Detached engine task: bootstrap + execute + finalize run here. Even
        // if the client disconnects immediately after the 202, this task keeps
        // running on `bus.bg_tasks` (TaskTracker) and the converter buffers
        // every emitted event into StreamRegistry — a later GET /stream
        // replays them. Bootstrap failure is surfaced as an Error event so the
        // UI sees it via the stream instead of a hanging 202, AND marks the
        // session `failed` + the stream `finished` so neither orphan-leaks.
        let ui_tx = bus.ui_event_tx.clone();
        let agent_for_broadcast = msg.agent_id.clone();
        let engine_cancel = pipeline_cancel.clone();
        let engine_for_task = engine.clone();
        let boot_msg = msg.clone();
        let db_for_task = infra.db.clone();
        let registry_for_task = bus.stream_registry.clone();
        let request_span = tracing::info_span!("sse_engine_turn");
        let engine_handle = bus.bg_tasks.spawn(
            async move {
                // Turn-level hard timeout: 10 minutes. Defense-in-depth against
                // runaway tool execution (MCP hang), stuck LLM streaming, or
                // any internal pipeline hang that bypasses the per-tool/per-call
                // timeouts. On expiry, cancels the engine task (which propagates
                // into execute() via the shared cancel token) so finalize can
                // mark the session interrupted instead of leaving it running
                // forever — the user sees an "interrupted" reply instead of a
                // spinner that never stops.
                const TURN_HARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);
                let turn_cancel = engine_cancel.clone();
                let turn_timer = tokio::spawn(async move {
                    tokio::time::sleep(TURN_HARD_TIMEOUT).await;
                    tracing::warn!(
                        "turn exceeded 1800s hard timeout — cancelling engine task"
                    );
                    turn_cancel.cancel();
                });

                let current_agent_name = engine_for_task.name().to_string();
                // Bootstrap INSIDE the task — no longer awaited by the POST
                // handler. If this fails, emit Error+Finish so the stream
                // surfaces it instead of hanging, mark the session `failed` so
                // the orphan row from the POST handler's create_new_session_with_id
                // doesn't leak, and mark the registry entry `finished` so
                // GET /stream subscribers don't block forever.
                let boot = match engine_for_task
                    .bootstrap_sse(&boot_msg, resume_session_id, force_new_session, model_override)
                    .await
                {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::error!(error = %e, "SSE bootstrap error (agent: {})", current_agent_name);
                        let sid_str = resp_session_id.to_string();
                        if let Err(e2) = opex_db::sessions::mark_session_failed(&db_for_task, resp_session_id).await {
                            tracing::warn!(error = %e2, session_id = %resp_session_id, "failed to mark session failed after bootstrap error");
                        }
                        let _ = engine_event_tx
                            .send_async(StreamEvent::Error(e.to_string()))
                            .await;
                        let _ = engine_event_tx
                            .send_async(StreamEvent::Finish {
                                finish_reason: "error".to_string(),
                                continuation: false,
                            })
                            .await;
                        registry_for_task.mark_finished(&sid_str).await;
                        turn_timer.abort();
                        return;
                    }
                };
                if let Err(e) = engine_for_task
                    .execute_sse(boot, engine_event_tx.clone(), engine_cancel, Some(job_id))
                    .await
                {
                    tracing::error!(error = %e, "SSE chat error (agent: {})", current_agent_name);
                    let sid_str = resp_session_id.to_string();
                    if let Err(e2) = opex_db::sessions::mark_session_failed(&db_for_task, resp_session_id).await {
                        tracing::warn!(error = %e2, session_id = %resp_session_id, "failed to mark session failed after execute_sse error");
                    }
                    let _ = engine_event_tx.send_async(StreamEvent::Error(e.to_string())).await;
                    let _ = engine_event_tx
                        .send_async(StreamEvent::Finish {
                            finish_reason: "error".to_string(),
                            continuation: false,
                        })
                        .await;
                    registry_for_task.mark_finished(&sid_str).await;
                    turn_timer.abort();
                    return;
                }

                // Normal completion: cancel the timeout so it doesn't fire after
                // the turn already finished.
                turn_timer.abort();

                let event = opex_types::ws::WsEvent::SessionUpdated {
                    agent: agent_for_broadcast,
                    session_id: None,
                    channel: Some(crate::agent::channel_kind::channel::UI.to_string()),
                };
                ui_tx.send(event.to_json()).ok();
            }
            .instrument(request_span),
        );

        let ctx = ConverterCtx {
            db: infra.db.clone(),
            invite_db: infra.db.clone(),
            registry: bus.stream_registry.clone(),
            sse_tx,
            pipeline_cancel,
            job_id,
            agent_name,
            mentioned_for_invite: mentioned_agent,
            user_text_for_title,
        };
        crate::trace_propagation::spawn_traced(run_converter(ctx, event_rx, engine_handle));

        return (
            StatusCode::ACCEPTED,
            Json(accepted_response_body(resp_session_id, resp_user_message_id)),
        )
            .into_response();
    }

    // Legacy synchronous path (no preallocated session_id) — keep the old
    // behaviour where bootstrap runs before 202 is sent. Used for legacy
    // requests without force_new_session AND without session_id (channels
    // don't hit this POST handler, so this is rare).
    let boot = match engine.bootstrap_sse(&msg, resume_session_id, force_new_session, model_override).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "SSE bootstrap error (agent: {agent_name})");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let resp_session_id = boot.session_id;
    let resp_user_message_id = boot.user_message_id;

    // Register the stream BEFORE responding — guarantees a subsequent
    //    GET /{id}/stream finds it. Capture `job_id` HERE (it used to be
    //    captured inside the converter's now-removed register block); it threads
    //    into ConverterCtx so the Finish/Error/exit `stream_jobs::set_content`
    //    persist (the resume content) still fires.
    let Some(job_id) = bus
        .stream_registry
        .register_with_token(
            resp_session_id,
            &agent_name,
            pipeline_cancel.clone(),
            resp_user_message_id,
        )
        .await
    else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "stream registry at capacity"})),
        )
            .into_response();
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
    // No SSE response is built on POST anymore. We still create the sse_tx the
    // converter writes to, but drop the receiver immediately: `send_and_buffer!`
    // no-ops the client send once `sse_tx.is_closed()` and keeps buffering into
    // the registry, and the converter's final `timeout(5s)` sends return
    // instantly on a closed channel. The reply is streamed by GET /{id}/stream
    // (T4), which subscribes to the same registry buffer.
    let (sse_tx, sse_rx) =
        tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(1024);
    drop(sse_rx);

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
    //     (broadcast-backed) regardless of whether any SSE client is connected
    //     — see `send_and_buffer!` in sse_converter.rs.
    //   * The ONLY path that aborts the engine task externally is the 30s grace
    //     window after an explicit user-initiated cancel (POST
    //     /api/chat/{id}/abort). Runaway protection is the engine's own
    //     (max_iterations, loop-detection, tool timeouts).
    //
    // Inter-agent communication happens via the `agent` tool (polling model),
    // so no turn loop is needed — a single execute_sse call suffices.
    let ui_tx = bus.ui_event_tx.clone();
    let agent_for_broadcast = msg.agent_id.clone();
    let invite_db = infra.db.clone();
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
    // directly let the HTTP request span close (handler returns while the engine
    // task keeps running) with the task still holding it — the next poll
    // re-entered a freed span id and tracing-subscriber's sharded registry
    // panicked on a worker thread. A child keeps the parent alive for the task's
    // lifetime and still inherits the extracted OTel parent context.
    // See `trace_propagation::spawn_traced` for the same fix + rationale.
    let request_span = tracing::info_span!("sse_engine_turn");
    let engine_handle = bus.bg_tasks.spawn(async move {
        let current_agent_name = engine.name().to_string();
        // T3: execute the already-bootstrapped turn. The stream is already
        // registered (above), so every event execute_sse emits lands in the
        // registry buffer.
        // T2: pass THIS turn's stream_job_id so the SessionLifecycleGuard can
        // ownership-gate its terminal run_status writes — a same-session
        // supersede must not let the OLD turn's finalize clobber the NEW turn's
        // just-claimed `running` row (and vice-versa).
        if let Err(e) = engine.execute_sse(boot, engine_event_tx.clone(), engine_cancel, Some(job_id)).await {
            tracing::error!(error = %e, "SSE chat error (agent: {})", current_agent_name);
            // Error is a non-text event — use send_async to honor CONTEXT.md
            // "non-text never dropped" contract. send_async awaits a slot on
            // the bounded channel and only errors if the channel is closed.
            let _ = engine_event_tx.send_async(StreamEvent::Error(e.to_string())).await;
        }

        // Notify UI about session update so sidebar refreshes
        let event = opex_types::ws::WsEvent::SessionUpdated {
            agent: agent_for_broadcast,
            session_id: None,
            channel: Some(crate::agent::channel_kind::channel::UI.to_string()),
        };
        ui_tx.send(event.to_json()).ok();
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
        job_id,
        agent_name,
        mentioned_for_invite: mentioned_agent,
        user_text_for_title,
    };
    crate::trace_propagation::spawn_traced(run_converter(ctx, event_rx, engine_handle));

    (
        StatusCode::ACCEPTED,
        Json(accepted_response_body(resp_session_id, resp_user_message_id)),
    )
        .into_response()
}

/// Build the `202 Accepted` body for `POST /api/chat`. Extracted so the exact
/// field names the client (T6) keys on are exercised by a unit test — a swapped
/// or renamed field fails `accepted_body_has_distinct_ids`.
fn accepted_response_body(session_id: uuid::Uuid, user_message_id: uuid::Uuid) -> serde_json::Value {
    json!({
        "session_id": session_id,
        "user_message_id": user_message_id,
    })
}

#[cfg(test)]
mod tests {
    use super::accepted_response_body;

    /// Exercises the real handler helper with two DISTINCT uuids so a
    /// swapped/renamed field is caught (asserting on a local literal would
    /// verify nothing about production).
    #[test]
    fn accepted_body_has_distinct_ids() {
        let session_id = uuid::Uuid::new_v4();
        let user_message_id = uuid::Uuid::new_v4();
        assert_ne!(session_id, user_message_id, "test needs distinct uuids");

        let body = accepted_response_body(session_id, user_message_id);
        assert_eq!(body["session_id"], session_id.to_string());
        assert_eq!(body["user_message_id"], user_message_id.to_string());
    }
}
