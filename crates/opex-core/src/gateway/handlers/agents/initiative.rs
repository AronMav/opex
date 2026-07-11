//! Stage C initiative endpoints: view plan, approve/dismiss proposals.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::gateway::state::AppState;
use super::validate_agent_name;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{name}/plan", get(api_get_plan))
        .route("/api/agents/{name}/plan/proposals/{id}/approve", post(api_approve_proposal))
        .route("/api/agents/{name}/plan/proposals/{id}/dismiss", post(api_dismiss_proposal))
}

async fn api_get_plan(
    State(app): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() || app.agents.get_engine(&name).await.is_none() {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    }
    let plan = crate::db::agent_plans::get_or_create(&app.infra.db, &name)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    let active = crate::db::session_goals::list_active_by_agent_and_origin(&app.infra.db, &name, "initiative")
        .await
        .unwrap_or_default();
    Ok(Json(json!({
        "agent": name,
        "current_focus": plan.current_focus,
        "proposals": plan.parsed_proposals(),
        "active_goals": active.iter().map(|g| json!({"goal": g.goal_text, "turns": g.turn_count})).collect::<Vec<_>>(),
    })))
}

async fn api_dismiss_proposal(
    State(app): State<AppState>,
    Path((name, id)): Path<(String, Uuid)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() || app.agents.get_engine(&name).await.is_none() {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    }
    let updated = crate::db::agent_plans::try_set_proposal_status(&app.infra.db, &name, id, "dismissed")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    // Idempotent: if it wasn't pending, still return ok (no-op).
    Ok(Json(json!({"ok": true, "changed": updated.is_some()})))
}

async fn api_approve_proposal(
    State(app): State<AppState>,
    Path((name, id)): Path<(String, Uuid)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "bad name"}))));
    }
    // get_engine returns Option<Arc<AgentEngine>> (gateway/clusters/agent_core.rs)
    // — the engine directly, not an AgentHandle.
    let Some(engine) = app.agents.get_engine(&name).await else {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    };
    // Initiative (Stage C self-proposed goals) is non-base only.
    if engine.cfg().agent.base {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "initiative is non-base only"}))));
    }
    // Atomic pending → approved; the goal text is resolved SERVER-SIDE from the
    // stored proposal — any text in the request body (there is none) is never
    // trusted here.
    let proposal = crate::db::agent_plans::try_set_proposal_status(&app.infra.db, &name, id, "approved")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    let Some(proposal) = proposal else {
        // Not pending (already acted on, or unknown id) → idempotent no-op, no spawn.
        return Ok(Json(json!({"ok": true, "spawned": false})));
    };

    // Spawn goal driver — mirrors scheduler::bootstrap_cron_goal, but with
    // origin='initiative' and no announce target (GoalTarget = None).
    const INITIATIVE_GOAL_MAX_TURNS: i32 = 20;
    let channel = crate::agent::channel_kind::channel::CRON; // reuse the system channel
    let session_id = crate::db::sessions::create_new_session(&app.infra.db, &name, "system", channel)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    crate::db::session_goals::upsert_initiative_goal(&app.infra.db, session_id, &proposal.text, INITIATIVE_GOAL_MAX_TURNS)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    if let Some(pool) = engine.cfg().goal_pool.clone() {
        let handle = crate::agent::goal::driver::spawn_goal_driver(engine.clone(), session_id, None);
        pool.insert(session_id, handle);
    }
    Ok(Json(json!({"ok": true, "spawned": true, "session_id": session_id})))
}
