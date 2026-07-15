//! Chat-related HTTP handlers split by route family.
//!
//! Composition entry point: [`routes`] returns a `Router<AppState>` that
//! merges the seven handlers across this module's sub-files. External
//! callers should use only [`routes`] and the cross-module re-export
//! [`set_model_override`] (mounted by `handlers::agents` so it can sit on
//! the `/api/agents/{name}/model-override` path next to the rest of agent
//! configuration).
//!
//! ```text
//! routes()
//!   ├── /health                       → misc::health
//!   ├── /v1/chat/completions          → openai_compat::chat_completions
//!   ├── /v1/models                    → models::list_models
//!   ├── /v1/embeddings                → embeddings::embeddings_proxy
//!   ├── /api/chat                     → sse::api_chat_sse
//!   ├── /api/chat/{id}/stream         → stream::api_chat_stream
//!   └── /api/chat/{id}/abort          → misc::api_chat_abort
//! ```

use axum::{
    Router,
    routing::{get, post},
};

use super::super::AppState;

mod embeddings;
mod misc;
mod models;
mod openai_compat;
mod sse;
mod sse_converter;
mod stream;
pub mod sse_writer;
mod streaming_db;

// Re-exported so `handlers::agents` can mount it under
// `/api/agents/{name}/model-override` without depending on `chat::misc`
// directly. Keeping the path stable as `super::chat::set_model_override`
// matches the pre-split visibility.
pub(crate) use misc::set_model_override;
// Same rationale, mounted under `/api/agents/{name}/context-breakdown` (T17).
pub(crate) use misc::api_context_breakdown;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(misc::health))
        .route("/v1/chat/completions", post(openai_compat::chat_completions))
        .route("/v1/models", get(models::list_models))
        .route("/v1/embeddings", post(embeddings::embeddings_proxy))
        .route("/api/chat", post(sse::api_chat_sse))
        .route("/api/chat/{id}/stream", get(stream::api_chat_stream))
        .route("/api/chat/{id}/abort", post(misc::api_chat_abort))
}
