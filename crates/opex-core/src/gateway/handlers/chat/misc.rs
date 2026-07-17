//! Small leaf handlers: `health`, `api_chat_abort`,
//! `set_model_override`, `api_context_breakdown`. Grouped here because each
//! is under ~25 lines and splitting them further produces files dominated by
//! their import blocks.

use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};

use crate::gateway::ApiError;
use crate::gateway::clusters::{AgentCore, ChannelBus, ConfigServices, InfraServices};
use crate::gateway::handlers::sessions::verify_session_agent;

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

    // Persist across restarts (T15 triage — the override previously lived
    // only in the in-memory ModelOverride, lost on every process restart).
    // Per-agent, not per-session — matches OPEX's existing in-memory semantics.
    let db = engine.cfg().db.clone();
    if let Err(e) = crate::db::model_overrides::set(&db, &agent_name, body.model.as_deref()).await {
        tracing::warn!(agent = %agent_name, error = %e, "failed to persist model override");
    }

    Json(serde_json::json!({"model": current})).into_response()
}

/// `GET /api/agents/{name}/context-breakdown` — estimate-only per-category
/// context-size breakdown (T17 triage, hermes parity for the `/usage`
/// popover). All values are chars/4 heuristics computed during the agent's
/// last turn (cached on `AgentState`), NOT provider-measured token counts —
/// mirrors the existing `system_prompt_size`/`context_size` log estimates in
/// `context_builder.rs`. `null` breakdown means no turn has run for this
/// agent since the process started.
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

/// POST /api/chat/{id}/abort?agent=xxx — cancel an in-progress stream.
///
/// `?agent=<owner>` is REQUIRED (audit 2026-07-04, IDOR): the bearer token
/// is shared across the whole instance, so without an owner check any
/// token-holder could cancel any other agent's in-flight turn by guessing
/// the session UUID (DoS on someone else's run). Matches the
/// `verify_session_agent` gate already enforced on every session endpoint
/// in `sessions.rs`.
pub(crate) async fn api_chat_abort(
    Path(session_id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(bus): State<ChannelBus>,
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    let agent = match params.get("agent").map(String::as_str) {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };

    let session_uuid = match uuid::Uuid::parse_str(&session_id) {
        Ok(u) => u,
        Err(_) => return ApiError::BadRequest("invalid session id".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, session_uuid, agent).await {
        return resp;
    }

    let cancelled = bus.stream_registry.cancel(&session_id).await;
    if cancelled {
        tracing::info!(session_id = %session_id, "stream cancelled via API");
        Json(json!({"ok": true, "message": "stream cancelled"})).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": "no active stream for this session"}))).into_response()
    }
}
