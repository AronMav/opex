//! `/api/usage` family — token-usage rollups from the `usage_log` table.
//! Time-windowed aggregates (default 30 days) for the dashboard and
//! per-session breakdowns for the agent details page.

use axum::{
    extract::{Query, State},
    response::Json,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::gateway::clusters::InfraServices;

#[derive(Debug, Deserialize)]
pub(crate) struct UsageQuery {
    days: Option<u32>,
    agent: Option<String>,
}

pub(crate) async fn api_usage(
    State(infra): State<InfraServices>,
    Query(q): Query<UsageQuery>,
) -> Json<Value> {
    let days = q.days.unwrap_or(30);
    match crate::db::usage::usage_summary(&infra.db, days).await {
        Ok(summary) => Json(json!({"ok": true, "days": days, "usage": summary})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub(crate) async fn api_usage_daily(
    State(infra): State<InfraServices>,
    Query(q): Query<UsageQuery>,
) -> Json<Value> {
    let days = q.days.unwrap_or(30);
    match crate::db::usage::usage_daily(&infra.db, days).await {
        Ok(daily) => Json(json!({"ok": true, "days": days, "daily": daily})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub(crate) async fn api_usage_sessions(
    State(infra): State<InfraServices>,
    Query(q): Query<UsageQuery>,
) -> Json<Value> {
    let days = q.days.unwrap_or(30);
    match crate::db::usage::usage_by_session(&infra.db, q.agent.as_deref(), days).await {
        Ok(sessions) => Json(json!({"ok": true, "days": days, "sessions": sessions})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}
