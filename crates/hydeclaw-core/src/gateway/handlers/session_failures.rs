//! Read-only API for the structured session failure log.
//!
//! Backed by `db::session_failures` (see migration 034). Exposes:
//!
//! - `GET /api/sessions/failures` — paginated list, optional `agent` filter.
//! - `GET /api/sessions/{session_id}/failures` — drill-down for one session.

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::session_failures::{
    self, SessionFailureRecord,
};
use crate::gateway::AppState;
use crate::gateway::clusters::InfraServices;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/sessions/failures", get(api_list_failures))
        .route("/api/sessions/{session_id}/failures", get(api_failures_for_session))
}

// ── DTO ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFailureDto {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub failed_at: DateTime<Utc>,
    pub failure_kind: String,
    pub error_message: String,
    pub last_tool_name: Option<String>,
    pub last_tool_output: Option<String>,
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
    pub iteration_count: Option<i32>,
    pub duration_secs: Option<i32>,
    pub context: Option<serde_json::Value>,
}

impl From<SessionFailureRecord> for SessionFailureDto {
    fn from(r: SessionFailureRecord) -> Self {
        Self {
            id: r.id,
            session_id: r.session_id,
            agent_id: r.agent_id,
            failed_at: r.failed_at,
            failure_kind: r.failure_kind,
            error_message: r.error_message,
            last_tool_name: r.last_tool_name,
            last_tool_output: r.last_tool_output,
            llm_provider: r.llm_provider,
            llm_model: r.llm_model,
            iteration_count: r.iteration_count,
            duration_secs: r.duration_secs,
            context: r.context_json,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SessionFailuresResponse {
    pub failures: Vec<SessionFailureDto>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

// ── Query params ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ListFailuresQuery {
    pub agent: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    20
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /api/sessions/failures?agent=&limit=20&offset=0`
pub(crate) async fn api_list_failures(
    State(infra): State<InfraServices>,
    Query(q): Query<ListFailuresQuery>,
) -> impl IntoResponse {
    let limit = q.limit.clamp(1, 100);
    let offset = q.offset.max(0);
    let agent = q.agent.as_deref().filter(|s| !s.is_empty());

    let failures = match session_failures::list_session_failures(&infra.db, agent, limit, offset).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let total = match session_failures::count_session_failures(&infra.db, agent).await {
        Ok(n) => n,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let dtos = failures.into_iter().map(SessionFailureDto::from).collect();
    Json(SessionFailuresResponse {
        failures: dtos,
        total,
        limit,
        offset,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionFailuresQuery {
    agent: Option<String>,
}

/// `GET /api/sessions/{session_id}/failures?agent=xxx`
///
/// Audit 2026-05-08 (6th pass): `?agent=` is required for the per-session
/// drill-down. Failure records carry diagnostic detail (error text,
/// stack-trace excerpts, tool names) that should not be readable by every
/// token-holder via UUID guessing. Cross-checks ownership through the same
/// `verify_session_agent` helper used by every other session-read endpoint.
pub(crate) async fn api_failures_for_session(
    State(infra): State<InfraServices>,
    Path(session_id): Path<Uuid>,
    Query(q): Query<SessionFailuresQuery>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "agent parameter required"})),
            )
                .into_response();
        }
    };
    // Reuse the cross-handler `verify_session_agent` semantics: 403 if the
    // session belongs to a different agent, 404 if the session is missing.
    let owner: Option<String> = match sqlx::query_scalar::<_, String>(
        "SELECT agent_id FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&infra.db)
    .await
    {
        Ok(o) => o,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match owner {
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "session not found"})),
            )
                .into_response();
        }
        Some(actual) if actual != agent => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "session belongs to a different agent"})),
            )
                .into_response();
        }
        Some(_) => {}
    }

    match session_failures::get_session_failures_for_session(&infra.db, session_id).await {
        Ok(rows) => {
            let dtos: Vec<SessionFailureDto> = rows.into_iter().map(SessionFailureDto::from).collect();
            Json(dtos).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
