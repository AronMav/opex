//! `POST /v1/embeddings` — proxy to configured embedding endpoint
//! (OpenAI-compatible). Delegates to `infra.embedder` which itself proxies
//! to Toolgate; no provider-specific knowledge here.

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};

use crate::gateway::clusters::InfraServices;

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
