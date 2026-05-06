//! Small leaf handlers: `health`, `mcp_callback`, `api_chat_abort`,
//! `set_model_override`. Grouped here because each is under ~25 lines and
//! splitting them further produces files dominated by their import blocks.

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};

use crate::gateway::clusters::{AgentCore, ChannelBus, ConfigServices, InfraServices};
use crate::tasks;

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
