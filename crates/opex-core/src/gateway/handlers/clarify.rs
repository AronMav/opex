//! `POST /api/clarify/{id}` — resolve a pending mid-run clarification waiter.
//!
//! Architecture: `ClarifyManager` is per-agent (held in `AgentConfig`).
//! `clarify_id` is a globally unique UUID, so we iterate over all running
//! engines and call `clarify_manager.resolve(id, response)` on each until
//! one succeeds. This mirrors the approach used by `approval_waiters` (which
//! look up the DB first to get the agent_id); here there is no DB record for
//! clarify entries — they are in-memory only — so we scan across engines
//! (n ≤ 20 in practice; resolution terminates at the first hit).

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json,
};
use uuid::Uuid;

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

    // Scan all running engines for the matching clarify waiter.
    // ClarifyManager::resolve() is sync (DashMap) and returns true on the first hit.
    let engines = agents_core.get_engines_map().await;
    for (_agent_name, engine) in &engines {
        if engine.cfg().clarify_manager.resolve(id, response.clone()) {
            return (
                StatusCode::OK,
                Json(serde_json::json!({"ok": true, "clarify_id": id})),
            )
                .into_response();
        }
    }

    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": format!("clarify {} not found or already resolved", id)})),
    )
        .into_response()
}
