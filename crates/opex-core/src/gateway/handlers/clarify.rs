//! `POST /api/clarify/{id}` — resolve a pending mid-run clarification waiter.
//!
//! Architecture: `ClarifyManager` is per-agent (held in `AgentConfig`).
//! `clarify_id` is a globally unique UUID; `CLARIFY_AGENT_INDEX` (process-wide
//! DashMap registered in `clarify_manager.rs`) maps each live clarify_id to its
//! owning agent name, so we can do a direct targeted lookup without scanning all
//! running engines.
//!
//! Web auth = single OPEX_AUTH_TOKEN (admin, no per-user web principal);
//! channel resolve is owner-gated separately. Targeted resolve avoids blind
//! cross-agent scan.

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json,
};
use uuid::Uuid;

use crate::agent::clarify_manager::agent_for_clarify;
use crate::gateway::clusters::AgentCore;
use crate::gateway::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/clarify/{id}", post(api_resolve_clarify))
}

/// POST /api/clarify/{id}
/// Body: `{"response": "<user answer>"}`
///
/// Resolves the pending clarify waiter identified by `id`. The engine is
/// currently blocked in `ClarifyManager::wait_rx`; delivering the response
/// unblocks it and allows the LLM loop to continue.
pub(crate) async fn api_resolve_clarify(
    State(agents_core): State<AgentCore>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let response = match body.get("response").and_then(|v| v.as_str()) {
        Some(r) => r.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "'response' field is required"})),
            )
                .into_response();
        }
    };

    // Targeted lookup via process-wide index: no iteration over all engines.
    let Some(agent_name) = agent_for_clarify(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("clarify {} not found or already resolved", id)})),
        )
            .into_response();
    };

    let Some(engine) = agents_core.get_engine(&agent_name).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("clarify {} not found or already resolved", id)})),
        )
            .into_response();
    };

    if engine.cfg().clarify_manager.resolve(id, response) {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "clarify_id": id})),
        )
            .into_response();
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": format!("clarify {} not found or already resolved", id)})),
    )
        .into_response()
}
