//! Topic-anchored reflection trigger (research §4). Lets the operator / UI ask
//! an agent to reflect on an explicit topic right now — a focused deep-dive that
//! bypasses the importance/cooldown trigger gate. The anchor handling itself
//! lives in the engine (`agent/soul/reflection.rs::maybe_reflect`); this module
//! is only the HTTP surface + dependency wiring.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::gateway::state::AppState;
use super::validate_agent_name;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/agents/{name}/reflect", post(api_reflect))
}

#[derive(Deserialize)]
struct ReflectPayload {
    anchor: String,
}

async fn api_reflect(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ReflectPayload>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "bad name"}))));
    }
    let Some(engine) = app.agents.get_engine(&name).await else {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    };
    // Anchored reflection is a no-op unless soul is enabled (reflection.rs gate)
    // — surface it explicitly instead of silently returning 202.
    if !engine.cfg().agent.soul.enabled {
        return Err((StatusCode::CONFLICT, Json(json!({"error": "soul not enabled for this agent"}))));
    }
    let anchor = body.anchor.trim().to_string();
    if anchor.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "anchor required"}))));
    }

    // Synthetic system session to attach the `reflection_cycle` timeline event to
    // (mirrors initiative::approve_proposal's session creation).
    let session_id = opex_db::sessions::create_new_session(
        &app.infra.db, &name, "system", crate::agent::channel_kind::channel::CRON,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;

    let db = app.infra.db.clone();
    let agent_name = engine.name().to_string();
    let provider = engine.provider_arc();
    let memory_store = engine.cfg().memory_store.clone();
    // SoulDeps built exactly as finalize_context_from_engine (pipeline/finalize.rs).
    let soul_deps = crate::agent::soul::reflection::SoulDeps {
        cfg: engine.cfg().agent.soul.clone(),
        workspace_dir: engine.cfg().workspace_dir.clone(),
        checkpoint: engine.cfg().checkpoint_manager.clone(),
        ui_event_tx: engine.state().ui_event_tx.clone(),
        runtime: engine.cfg().soul_runtime.clone(),
        emotion: engine.cfg().agent.emotion.clone(),
    };

    // Spawn on the per-agent tracker — the same lifecycle as the auto reflection
    // path (knowledge extraction → maybe_reflect), drained on graceful shutdown.
    engine.state().bg_tasks.spawn(async move {
        crate::agent::soul::reflection::maybe_reflect(
            &db, &agent_name, session_id, &provider, &memory_store, &soul_deps,
            0.0, // threshold_bias is immaterial — the anchor bypasses the trigger gate
            Some(anchor),
        )
        .await;
    });

    Ok((StatusCode::ACCEPTED, Json(json!({"ok": true, "queued": true, "session_id": session_id}))))
}
