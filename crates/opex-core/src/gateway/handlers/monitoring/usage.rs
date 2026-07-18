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
}

/// `GET /api/tools/health` — failing tools ordered by impact (worst first).
/// Operator-facing degradation report over `tool_quality`; complements
/// `/api/doctor`'s `get_degraded_tools` with the raw counters.
pub(crate) async fn api_tools_health(State(infra): State<InfraServices>) -> Json<Value> {
    match crate::db::tool_quality::get_tool_health(&infra.db).await {
        Ok(tools) => Json(json!({ "tools": tools })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn api_usage(
    State(infra): State<InfraServices>,
    Query(q): Query<UsageQuery>,
) -> Json<Value> {
    let days = q.days.unwrap_or(30);
    match crate::db::usage::usage_summary(&infra.db, days).await {
        Ok(mut summary) => {
            // Phase 3: recompute cost from the model catalog (5000+ models) when
            // it has real pricing — more accurate + far broader than opex-db's
            // built-in fallback price table. Rows the catalog doesn't know keep
            // the built-in estimate.
            for row in &mut summary {
                if let Some(c) = catalog_cost(&row.provider, &row.model, row.total_input, row.total_output) {
                    row.estimated_cost = Some(c);
                }
            }
            Json(json!({"ok": true, "days": days, "usage": summary}))
        }
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// Cost (USD) for `input`/`output` tokens from the catalog's per-1M pricing.
fn catalog_cost(provider: &str, model: &str, input: i64, output: i64) -> Option<f64> {
    let c = opex_catalog::global_cost(provider, model)?;
    let cost = (input.max(0) as f64 / 1_000_000.0) * c.input
        + (output.max(0) as f64 / 1_000_000.0) * c.output;
    Some((cost * 10000.0).round() / 10000.0)
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
