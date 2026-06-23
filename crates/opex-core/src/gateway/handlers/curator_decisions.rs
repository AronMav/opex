use axum::{
    Router,
    extract::{Path, Query, State},
    response::{IntoResponse, Json},
    routing::get,
};
use serde::Deserialize;
use std::collections::HashMap;
use super::super::AppState;
use crate::gateway::clusters::InfraServices;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/curator-decisions/recent", get(api_curator_decisions_recent))
        .route("/api/skills/{skill}/curator-decisions", get(api_skill_curator_decisions))
}

/// GET /api/curator-decisions/recent
/// Returns the most recent curator decision per skill as a flat map {skill_name: decision}.
/// Skills with no decisions are absent from the response.
pub(crate) async fn api_curator_decisions_recent(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    match crate::db::curator_decisions::list_recent(&infra.db).await {
        Ok(rows) => {
            let map: HashMap<String, serde_json::Value> = rows.into_iter()
                .map(|r| (r.skill_name.clone(), serde_json::json!({
                    "action":     r.action,
                    "reason":     r.reason,
                    "decided_at": r.decided_at,
                })))
                .collect();
            Json(map).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "curator-decisions/recent query failed");
            Json(serde_json::json!({})).into_response()
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct DecisionsQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 { 5 }

/// GET /api/skills/{skill}/curator-decisions?limit=5
/// Returns the last N decisions for a specific skill.
pub(crate) async fn api_skill_curator_decisions(
    State(infra): State<InfraServices>,
    Path(skill): Path<String>,
    Query(params): Query<DecisionsQuery>,
) -> impl IntoResponse {
    let limit = params.limit.clamp(1, 50);
    match crate::db::curator_decisions::list_decisions(&infra.db, &skill, limit).await {
        Ok(rows) => Json(serde_json::json!({ "decisions": rows })).into_response(),
        Err(e) => {
            tracing::warn!(skill, error = %e, "skill curator-decisions query failed");
            Json(serde_json::json!({ "decisions": [] })).into_response()
        }
    }
}
