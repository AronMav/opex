//! Read-only API for the structured session failure log.
//!
//! Backed by `db::session_failures` (see migration 034). Exposes:
//!
//! - `GET /api/sessions/failures` — paginated list, optional `agent` filter.
//! - `GET /api/sessions/{session_id}/failures` — drill-down for one session.

use axum::{
    Router,
    extract::{Query, State},
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
///
/// Audit 2026-05-08 (7th pass): `?agent=` is now MANDATORY. Without the
/// filter the endpoint returned the full failure log across every agent,
/// including diagnostic detail (error_message, last_tool_output, llm_model,
/// context). With every other session-read endpoint gated on `?agent=`,
/// leaving the bulk-list open was inconsistent and let a token-holder
/// enumerate every agent's failures.
pub(crate) async fn api_list_failures(
    State(infra): State<InfraServices>,
    Query(q): Query<ListFailuresQuery>,
) -> impl IntoResponse {
    let limit = q.limit.clamp(1, 100);
    let offset = q.offset.max(0);
    let agent = match q.agent.as_deref().filter(|s| !s.is_empty()) {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "agent parameter required"})),
            )
                .into_response();
        }
    };

    let failures = match session_failures::list_session_failures(&infra.db, Some(agent), limit, offset).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let total = match session_failures::count_session_failures(&infra.db, Some(agent)).await {
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
