//! `/api/audit` and `/api/audit/tools` — read-only access to the
//! `audit_log` and `tool_audit` tables for the admin UI.

use axum::{
    extract::{Query, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::gateway::clusters::InfraServices;

#[derive(Deserialize)]
pub(crate) struct AuditQuery {
    agent: Option<String>,
    event_type: Option<String>,
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(crate) async fn api_audit_events(
    State(infra): State<InfraServices>,
    Query(q): Query<AuditQuery>,
) -> Json<Value> {
    let limit = q.limit.unwrap_or(100).min(500);
    let offset = q.offset.unwrap_or(0);
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match crate::db::audit::query_events(
        &infra.db,
        q.agent.as_deref(),
        q.event_type.as_deref(),
        search,
        limit,
        offset,
    ).await {
        Ok(events) => Json(json!({"ok": true, "events": events, "limit": limit, "offset": offset})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}
