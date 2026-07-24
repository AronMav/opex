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
    /// Agent to route to (`OPEX` extension). Defaults to first available.
    agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionChunk {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<ChatChunkChoice>,
}

#[derive(Debug, Serialize)]
struct ChatChunkChoice {
    index: u32,
    delta: ChatChunkDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct ChatChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<ChatResponseChoice>,
    usage: ChatResponseUsage,
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

#[derive(Debug, Default, Serialize)]
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
    // F069: echo the REQUESTED identifier (the agent id advertised by
    // /v1/models and passed in `model`), not engine.model_name() (the provider
    // model, e.g. "glm-4.6"). OpenAI-compat routers correlate response.model
    // with the requested model; the provider-model mismatch broke them.
    let model_name = req
        .model
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| req.agent.clone().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| engine.model_name());
    let created = chrono::Utc::now().timestamp();

    if req.stream {
        let (sse_tx, sse_rx) =
            tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(1024);

        let messages = req.messages.clone();
        // H6 fix: route through `engine.state().bg_tasks` (TaskTracker) instead
        // of bare `tokio::spawn` so graceful shutdown awaits these tasks. The
        // OpenAI-compat path used to leak detached tasks on SIGTERM — the
        // runtime dropped them mid-LLM-call, losing in-flight state.
        let outer_bg_tasks = engine.state().bg_tasks.clone();
        outer_bg_tasks.spawn(async move {
            let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<String>(1024);

            let engine_clone = engine.clone();
            let inner_bg_tasks = engine_clone.state().bg_tasks.clone();
            let handle = inner_bg_tasks.spawn(async move {
                engine_clone.handle_openai(&messages, Some(chunk_tx)).await
            });

            // F013: abort the run when the client disconnects. `sse_rx` is
            // dropped by axum on disconnect, so `sse_tx.closed()` resolves —
            // select on it alongside chunk delivery so an aborted request stops
            // burning LLM calls / tool executions immediately, even mid tool
            // loop (the old `while let ... recv()` only noticed on the next
            // chunk and never aborted the detached inner task).
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = sse_tx.closed() => {
                        handle.abort();
                        return;
                    }
                    maybe = chunk_rx.recv() => match maybe {
                        Some(c) => c,
                        None => break, // engine finished — emit the final stop chunk
                    },
                };
                let payload = ChatCompletionChunk {
                    id: completion_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model_name.clone(),
                    choices: vec![ChatChunkChoice {
                        index: 0,
                        delta: ChatChunkDelta {
                            content: Some(chunk),
                        },
                        finish_reason: None,
                    }],
                };
                let data = serde_json::to_string(&payload)
                    .expect("ChatCompletionChunk serialization is infallible");
                sse_tx.try_send(Ok(Event::default().data(data))).ok();
            }

            // Final stop chunk
            let payload = ChatCompletionChunk {
                id: completion_id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model_name.clone(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta::default(),
                    finish_reason: Some("stop".to_string()),
                }],
            };
            let data = serde_json::to_string(&payload)
                .expect("ChatCompletionChunk serialization is infallible");
            sse_tx.try_send(Ok(Event::default().data(data))).ok();
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
            // OpenAI always returns a `usage` object — emit a zero-filled one
            // when the provider didn't report token counts (was `null` before,
            // which strict OpenAI clients/SDKs choke on).
            let usage = llm_resp.usage.map_or_else(ChatResponseUsage::default, |u| {
                ChatResponseUsage {
                    prompt_tokens: u.input_tokens,
                    completion_tokens: u.output_tokens,
                    total_tokens: u.input_tokens + u.output_tokens,
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_text_delta_serializes_byte_equal() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1700000000,
            model: "Opex".to_string(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatChunkDelta {
                    content: Some("hello".to_string()),
                },
                finish_reason: None,
            }],
        };
        let actual = serde_json::to_string(&chunk).unwrap();
        let expected = r#"{"id":"chatcmpl-test","object":"chat.completion.chunk","created":1700000000,"model":"Opex","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        assert_eq!(actual, expected);
    }

    #[test]
    fn chunk_final_stop_serializes_byte_equal() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1700000000,
            model: "Opex".to_string(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatChunkDelta::default(),
                finish_reason: Some("stop".to_string()),
            }],
        };
        let actual = serde_json::to_string(&chunk).unwrap();
        let expected = r#"{"id":"chatcmpl-test","object":"chat.completion.chunk","created":1700000000,"model":"Opex","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        assert_eq!(actual, expected);
    }
}

