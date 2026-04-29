use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{
        IntoResponse, Json,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::str::FromStr;
use tokio_util::sync::CancellationToken;

use super::super::{AppState, OpenAiMessage, sse_types};
use crate::agent::engine::StreamEvent;
use crate::gateway::clusters::{AgentCore, ChannelBus, ConfigServices, InfraServices};
use crate::tasks;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/api/mcp/callback", post(mcp_callback))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/v1/embeddings", post(embeddings_proxy))
        .route("/api/chat", post(api_chat_sse))
        .route("/api/chat/{id}/stream", get(api_chat_resume_stream))
        .route("/api/chat/{id}/abort", post(api_chat_abort))
}

// ── Streaming message RAII guard ──
// Ensures streaming messages are finalized in DB even if the converter task
// panics or exits unexpectedly (e.g. engine panic, tokio cancellation).

struct StreamingMessageGuard {
    db: sqlx::PgPool,
    msg_id: uuid::Uuid,
    session_id: Option<uuid::Uuid>,
    finalized: bool,
}

impl StreamingMessageGuard {
    fn new(db: sqlx::PgPool, msg_id: uuid::Uuid) -> Self {
        Self { db, msg_id, session_id: None, finalized: false }
    }
    fn set_session_id(&mut self, sid: uuid::Uuid) {
        self.session_id = Some(sid);
    }
    fn mark_finalized(&mut self) {
        self.finalized = true;
    }
}

impl Drop for StreamingMessageGuard {
    fn drop(&mut self) {
        if !self.finalized
            && let Some(_sid) = self.session_id {
                let db = self.db.clone();
                let mid = self.msg_id;
                tokio::spawn(async move {
                    if let Err(e) = crate::db::sessions::finalize_streaming_message(&db, mid).await {
                        tracing::warn!(error = %e, msg_id = %mid, "failed to finalize streaming message in guard Drop");
                    }
                });
            }
    }
}

// ── SSE flush helpers (bounded text accumulation + delta tools) ──

/// Build tools JSON from accumulated tools, reusing cached value when no new tools arrived.
/// Only calls `.to_vec()` when `accumulated_tools` actually grew since the last build.
fn build_tools_json(
    tools: &[serde_json::Value],
    flushed_count: &mut usize,
    cache: &mut Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    if tools.is_empty() {
        return None;
    }
    if cache.is_none() || tools.len() != *flushed_count {
        *cache = Some(serde_json::Value::Array(tools.to_vec()));
        *flushed_count = tools.len();
    }
    cache.clone()
}

/// Append-mode streaming message upsert. Text is APPENDED to existing content (not replaced).
/// Used for bounded text accumulation -- caller clears `accumulated_text` after success.
/// Also touches session activity for watchdog heartbeat, mirroring `upsert_streaming_message` behavior.
///
/// Invariant (Bug 2 fix, 2026-04-20): on INSERT we anchor `parent_message_id`
/// to the most-recent `role='user'` row for this session via a correlated
/// subquery. `bootstrap::run` persists the user row BEFORE the streaming row
/// is ever written, so the subquery is guaranteed to find a candidate.
/// `ON CONFLICT DO UPDATE` continues to append (`content || $3`) and refresh
/// `tool_calls`, but it deliberately does NOT touch `parent_message_id` —
/// the parent is pinned at first INSERT.
async fn upsert_streaming_append(
    db: &sqlx::PgPool,
    message_id: uuid::Uuid,
    session_id: uuid::Uuid,
    agent_id: &str,
    text_delta: &str,
    tool_calls: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, tool_calls, agent_id, status, parent_message_id) \
         VALUES ( \
             $1, $2, 'assistant', $3, $4, $5, 'streaming', \
             (SELECT id FROM messages \
              WHERE session_id = $2 AND role = 'user' \
              ORDER BY created_at DESC \
              LIMIT 1) \
         ) \
         ON CONFLICT (id) DO UPDATE SET content = messages.content || $3, tool_calls = $4",
    )
    .bind(message_id)
    .bind(session_id)
    .bind(text_delta)
    .bind(tool_calls)
    .bind(agent_id)
    .execute(db)
    .await?;
    // Maintain watchdog heartbeat -- mirrors what upsert_streaming_message does today.
    crate::db::sessions::touch_session_activity(db, session_id)
        .await
        .ok();
    Ok(())
}

/// Read the accumulated content from a streaming message row.
/// Used at Finish/Error/unexpected-exit to get full text for `stream_jobs` `set_content`,
/// since `accumulated_text` is cleared after each periodic flush.
async fn read_streaming_content(db: &sqlx::PgPool, message_id: uuid::Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT COALESCE(content, '') FROM messages WHERE id = $1")
        .bind(message_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

// ── OpenAI-compatible /v1/chat/completions ──

#[allow(dead_code)] // Deserialized from JSON; model/temperature reserved for future use
#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionRequest {
    model: Option<String>,
    messages: Vec<OpenAiMessage>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    stream: bool,
    /// Agent to route to (`HydeClaw` extension). Defaults to first available.
    agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<ChatResponseChoice>,
    usage: Option<ChatResponseUsage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools_used: Vec<String>,
    iterations: u32,
}

#[derive(Debug, Serialize)]
struct ChatResponseChoice {
    index: u32,
    message: ChatResponseMessage,
    finish_reason: String,
}

#[derive(Debug, Serialize)]
struct ChatResponseMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatResponseUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

pub(crate) async fn chat_completions(
    State(agents): State<AgentCore>,
    Json(req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    // Route to agent: req.agent extension first, then req.model as agent name, then first available
    let engine = {
        let by_ext = req.agent.as_deref().filter(|s| !s.is_empty());
        let by_model = req.model.as_deref().filter(|s| !s.is_empty());
        match (by_ext, by_model) {
            (Some(name), _) => agents.get_engine(name).await,
            (None, Some(name)) => {
                let e = agents.get_engine(name).await;
                if e.is_some() { e } else { agents.first_engine().await }
            }
            _ => agents.first_engine().await,
        }
    };

    let engine = match engine {
        Some(e) => e,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": "no agent available", "type": "invalid_request_error"}})),
            )
                .into_response();
        }
    };

    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let model_name = engine.model_name();
    let created = chrono::Utc::now().timestamp();

    if req.stream {
        let (sse_tx, sse_rx) =
            tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(1024);

        let messages = req.messages.clone();
        tokio::spawn(async move {
            let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

            let engine_clone = engine.clone();
            let handle = tokio::spawn(async move {
                engine_clone.handle_openai(&messages, Some(chunk_tx)).await
            });

            while let Some(chunk) = chunk_rx.recv().await {
                let data = json!({
                    "id": completion_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model_name,
                    "choices": [{"index": 0, "delta": {"content": chunk}, "finish_reason": null}]
                });
                sse_tx.try_send(Ok(Event::default().data(data.to_string()))).ok();
            }

            // Final stop chunk
            let data = json!({
                "id": completion_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_name,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            });
            sse_tx.try_send(Ok(Event::default().data(data.to_string()))).ok();
            sse_tx.try_send(Ok(Event::default().data("[DONE]"))).ok();

            if let Ok(Err(e)) = handle.await {
                tracing::error!(error = %e, "streaming chat completion error");
            }
        });

        return Sse::new(tokio_stream::wrappers::ReceiverStream::new(sse_rx))
            .keep_alive(KeepAlive::default())
            .into_response();
    }

    // Non-streaming: pass full message history to handle_openai
    match engine.handle_openai(&req.messages, None).await {
        Ok(llm_resp) => {
            let usage = llm_resp.usage.map(|u| ChatResponseUsage {
                prompt_tokens: u.input_tokens,
                completion_tokens: u.output_tokens,
                total_tokens: u.input_tokens + u.output_tokens,
            });
            let resp = ChatCompletionResponse {
                id: completion_id,
                object: "chat.completion".to_string(),
                created,
                model: model_name,
                choices: vec![ChatResponseChoice {
                    index: 0,
                    message: ChatResponseMessage {
                        role: "assistant".to_string(),
                        content: llm_resp.content,
                    },
                    finish_reason: "stop".to_string(),
                }],
                usage,
                tools_used: llm_resp.tools_used,
                iterations: llm_resp.iterations,
            };
            Json(resp).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "chat completion error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": e.to_string(), "type": "server_error"}})),
            )
                .into_response()
        }
    }
}

// ── GET /v1/models ──

pub(crate) async fn list_models(State(agents): State<AgentCore>) -> Json<Value> {
    let agents_map = agents.map.read().await;
    let data: Vec<Value> = agents_map
        .keys()
        .map(|name| {
            json!({
                "id": name,
                "object": "model",
                "created": 0,
                "owned_by": "hydeclaw"
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": data }))
}

/// POST /v1/embeddings — proxy to configured embedding endpoint (OpenAI-compatible).
pub(crate) async fn embeddings_proxy(
    State(infra): State<InfraServices>,
    Json(req): Json<Value>,
) -> impl IntoResponse {
    if !infra.embedder.is_available() {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({
            "error": {"message": "embeddings not configured", "type": "server_error"}
        }))).into_response();
    }

    let input = req.get("input").cloned().unwrap_or(json!(""));
    let texts: Vec<String> = if let Some(arr) = input.as_array() {
        arr.iter().filter_map(|v| v.as_str().map(std::string::ToString::to_string)).collect()
    } else if let Some(s) = input.as_str() {
        vec![s.to_string()]
    } else {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": {"message": "input must be a string or array of strings", "type": "invalid_request_error"}
        }))).into_response();
    };

    if texts.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": {"message": "input must not be empty", "type": "invalid_request_error"}
        }))).into_response();
    }

    let refs: Vec<&str> = texts.iter().map(std::string::String::as_str).collect();
    match infra.embedder.embed_batch(&refs).await {
        Ok(embeddings) => {
            let data: Vec<Value> = embeddings.iter().enumerate().map(|(i, emb)| {
                json!({"object": "embedding", "index": i, "embedding": emb})
            }).collect();
            Json(json!({
                "object": "list",
                "data": data,
                "model": infra.embedder.embed_model_name().unwrap_or_default(),
                "usage": {"prompt_tokens": 0, "total_tokens": 0}
            })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
            "error": {"message": e.to_string(), "type": "server_error"}
        }))).into_response(),
    }
}

// ── AI SDK SSE Chat endpoint ──

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
        attachments: vec![],
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
    });

    // Converter task: StreamEvent → SSE JSON events (Vercel AI SDK v3 UI format)
    // Based on @ai-sdk/react UIMessageChunk types
    // Also buffers events in StreamRegistry for resume support.
    //
    // AUDIT:SSE-01 (verified 2026-03-30): Event ordering is guaranteed by single-task
    // sequential processing. The `while let Some(event) = event_rx.recv().await` loop
    // is the sole consumer of engine events. `pending_text_end` ensures text-end is
    // flushed before any non-text event (Finish, Error, ToolCallStart) -- see top of
    // loop (line ~469) and explicit flush in Finish handler. No concurrent emission
    // possible because event_rx.recv() processes one event at a time in this task.
    //
    // AUDIT:SSE-02 (verified 2026-03-30): Error delivery has two paths:
    // 1. LLM errors mid-stream: engine sends error as TextDelta via format_user_error()
    //    (intentional -- user sees error inline in chat history), then Finish event.
    // 2. handle_sse() top-level errors: chat.rs sends StreamEvent::Error via event_tx,
    //    converter sends error SSE event, marks stream as error in registry, finalizes
    //    the streaming message, then sends [DONE]. Client always receives error before
    //    connection close in both paths.
    let registry = bus.stream_registry.clone();
    tokio::spawn(async move {
        let mut text_id_counter: usize = 0;
        let mut pending_text_end: Option<String> = None;
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
                if let Some(ref sid) = session_id_str {
                    registry.push_event(sid, &$json_str).await;
                }
                if !sse_tx.is_closed() {
                    client_gone_since = None;
                    sse_tx.try_send(Ok(Event::default().data($json_str))).is_ok()
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
            use super::cancel_grace::{
                poll_event_with_cancel_grace, GracePollResult,
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
            // If there's a pending text-end needed, send it first
            if let Some(text_id) = pending_text_end.take() {
                let end_data = json!({"type": sse_types::TEXT_END, "id": text_id}).to_string();
                let _ = send_and_buffer!(end_data);
            }

            let data = match event {
                StreamEvent::SessionId(sid) => {
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
                    json!({"type": sse_types::DATA_SESSION_ID, "data": {"sessionId": sid}, "transient": true})
                }
                StreamEvent::MessageStart { message_id } => {
                    json!({"type": sse_types::START, "messageId": message_id, "agentName": current_responding_agent})
                }
                StreamEvent::StepStart { step_id: _ } => {
                    continue;
                }
                StreamEvent::TextDelta(ref text) => {
                    if session_uuid.is_none() && accumulated_text.is_empty() {
                        tracing::error!("TextDelta received but session_uuid is None — DB flush will be skipped");
                    }
                    // AI SDK v3: text-start → text-delta → text-end
                    text_id_counter += 1;
                    let text_id = format!("text-{text_id_counter}");
                    let start_data = json!({"type": sse_types::TEXT_START, "id": text_id.clone(), "agentName": current_responding_agent}).to_string();
                    let delta_data = json!({"type": sse_types::TEXT_DELTA, "id": text_id.clone(), "delta": text}).to_string();
                    let _ = send_and_buffer!(start_data);
                    let _ = send_and_buffer!(delta_data);
                    pending_text_end = Some(text_id);
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
                    // Send any pending text-end first
                    if let Some(text_id) = pending_text_end.take() {
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
            if let Some(text_id) = pending_text_end {
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
            tokio::spawn(async move {
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

// ── Stream Resume endpoint ──

/// Resume an active SSE stream by session ID.
/// AI SDK calls GET /api/chat/{id}/stream on mount when resume=true.
/// Returns 204 if no active stream, or SSE with replay + live events.
pub(crate) async fn api_chat_resume_stream(
    Path(id): Path<String>,
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    use async_stream::stream;
    use tokio::sync::broadcast;

    match bus.stream_registry.subscribe(&id).await {
        None => {
            // No in-memory stream — check DB for recently finished/interrupted job
            let session_uuid = uuid::Uuid::parse_str(&id).ok();
            if let Some(sid) = session_uuid
                && let Ok(Some(job)) = crate::gateway::stream_jobs::get_active_job(
                    bus.stream_registry.db(), sid
                ).await {
                    let sync_status = match job.status.as_str() {
                        "finished" => "finished",
                        "error" => "error",
                        "running" => {
                            // Running in DB but not in memory = Core restarted mid-stream
                            if let Err(e) = crate::gateway::stream_jobs::error_job(
                                bus.stream_registry.db(), job.id, "stream lost: core restarted"
                            ).await {
                                tracing::warn!(error = %e, "failed to mark stream job as error on resume");
                            }
                            "interrupted"
                        }
                        _ => "error",
                    };
                    let sync = serde_json::json!({
                        "type": sse_types::SYNC,
                        "content": job.aggregated_text,
                        "toolCalls": job.tool_calls,
                        "status": sync_status,
                        "error": job.error_text,
                    });
                    let sync_str = sync.to_string();
                    let sse_stream = async_stream::stream! {
                        yield Ok::<_, std::convert::Infallible>(Event::default().data(sync_str));
                        yield Ok(Event::default().data("[DONE]"));
                    };
                    return Sse::new(sse_stream)
                        .keep_alive(KeepAlive::default())
                        .into_response();
                }
            StatusCode::NO_CONTENT.into_response()
        }
        Some((buffered_events, mut broadcast_rx, already_finished)) => {
            let replay_count = buffered_events.len();

            let sse_stream = stream! {
                // Phase 1: Replay buffered events
                for (_seq, event_json) in buffered_events {
                    yield Ok::<_, std::convert::Infallible>(
                        Event::default().data(event_json)
                    );
                }

                if already_finished {
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }

                // Phase 2: Live events via broadcast subscription
                // Events between subscribe() and here may overlap with buffer — skip them
                let mut skip_remaining = replay_count;
                loop {
                    match broadcast_rx.recv().await {
                        Ok((_seq, event_json)) => {
                            if skip_remaining > 0 {
                                skip_remaining -= 1;
                                continue;
                            }
                            let is_terminal =
                                event_json.contains("\"type\":\"finish\"")
                                || event_json.contains("\"type\":\"error\"");
                            yield Ok(Event::default().data(event_json));
                            if is_terminal {
                                yield Ok(Event::default().data("[DONE]"));
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                lagged = n,
                                session = %id,
                                "Resume stream lagged"
                            );
                            skip_remaining = skip_remaining.saturating_sub(n as usize);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
            };

            (
                [(
                    axum::http::header::HeaderName::from_static(
                        "x-vercel-ai-ui-message-stream",
                    ),
                    "v1",
                )],
                Sse::new(sse_stream).keep_alive(KeepAlive::default()),
            )
                .into_response()
        }
    }
}

// ── Per-session model override ──

#[derive(Debug, serde::Deserialize)]
pub(crate) struct ModelOverrideBody {
    model: Option<String>,
}

pub(crate) async fn set_model_override(
    State(agents): State<AgentCore>,
    Path(agent_name): Path<String>,
    Json(body): Json<ModelOverrideBody>,
) -> impl IntoResponse {
    let Some(engine) = agents.get_engine(&agent_name).await else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"not found"}))).into_response();
    };
    engine.set_model_override(body.model.clone());
    let current = engine.current_model();
    Json(serde_json::json!({"model": current})).into_response()
}

pub(crate) async fn health(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
) -> Json<Value> {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&infra.db)
        .await
        .is_ok();

    let config = cfg.shared_config.read().await;

    // Agent names and icons are intentionally omitted here — /health is unauthenticated
    // and must not leak information about which agents are configured.
    // Authenticated callers should use GET /api/agents instead.
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "db": db_ok,
        "listen": config.gateway.listen,
    }))
}

pub(crate) async fn mcp_callback(
    State(infra): State<InfraServices>,
    Json(payload): Json<hydeclaw_types::McpCallback>,
) -> StatusCode {
    tracing::info!(
        task_id = %payload.task_id,
        status = %payload.status,
        "MCP callback received"
    );

    if let Err(e) = tasks::update_step_from_callback(&infra.db, &payload).await {
        tracing::error!(error = %e, "failed to process MCP callback");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

/// POST /api/chat/{id}/abort — cancel an in-progress stream from any client.
pub(crate) async fn api_chat_abort(
    Path(session_id): Path<String>,
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    let cancelled = bus.stream_registry.cancel(&session_id).await;
    if cancelled {
        tracing::info!(session_id = %session_id, "stream cancelled via API");
        Json(json!({"ok": true, "message": "stream cancelled"})).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": "no active stream for this session"}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_tools_json_empty_returns_none() {
        let mut count = 0usize;
        let mut cache = None;
        assert!(build_tools_json(&[], &mut count, &mut cache).is_none());
    }

    #[test]
    fn build_tools_json_first_call_builds_array() {
        let tools = vec![serde_json::json!({"name": "search"})];
        let mut count = 0usize;
        let mut cache = None;
        let result = build_tools_json(&tools, &mut count, &mut cache).unwrap();
        assert_eq!(result, serde_json::json!([{"name": "search"}]));
        assert_eq!(count, 1);
    }

    #[test]
    fn build_tools_json_same_count_reuses_cache() {
        let tools = vec![serde_json::json!({"name": "search"})];
        let mut count = 0usize;
        let mut cache = None;
        build_tools_json(&tools, &mut count, &mut cache);
        let sentinel = serde_json::json!("SENTINEL");
        cache = Some(sentinel.clone());
        // Same count → reuse cache, not rebuild
        let result = build_tools_json(&tools, &mut count, &mut cache).unwrap();
        assert_eq!(result, sentinel);
    }

    #[test]
    fn build_tools_json_new_tool_invalidates_cache() {
        let tools_1 = vec![serde_json::json!({"name": "search"})];
        let mut count = 0usize;
        let mut cache = None;
        build_tools_json(&tools_1, &mut count, &mut cache);

        let tools_2 = vec![
            serde_json::json!({"name": "search"}),
            serde_json::json!({"name": "write"}),
        ];
        let result = build_tools_json(&tools_2, &mut count, &mut cache).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 2);
        assert_eq!(count, 2);
    }
}
