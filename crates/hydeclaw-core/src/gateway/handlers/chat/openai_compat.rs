//! OpenAI-compatible `POST /v1/chat/completions` endpoint.
//!
//! Two response shapes:
//! - non-streaming JSON (`{ id, object, created, model, choices, usage, ... }`)
//! - SSE streaming (`text/event-stream` with `chat.completion.chunk` deltas
//!   and a terminal `[DONE]` line)
//!
//! Routing precedence: `req.agent` extension → `req.model` as agent name →
//! first available agent.

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

use super::super::super::OpenAiMessage;
use crate::gateway::clusters::AgentCore;

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

