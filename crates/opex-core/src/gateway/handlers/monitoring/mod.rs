//! Monitoring + diagnostics HTTP handlers.
//!
//! Composes the `/api/setup/*`, `/api/status`, `/api/stats`,
//! `/api/usage/*`, `/api/doctor`, `/api/health/dashboard`, `/api/audit/*`
//! and `/api/watchdog/*` route families. Every endpoint is read-only or
//! infrequently-mutating; nothing here participates in the LLM hot path.
//!
//! Sub-modules:
//! - [`setup`] — wizard (`/api/setup/{status,complete,requirements}` +
//!   the post-setup guard middleware)
//! - [`status`] — `/api/status`, `/api/stats`
//! - [`usage`] — `/api/usage`, `/api/usage/daily`, `/api/usage/sessions`
//! - [`audit`] — `/api/audit`, `/api/audit/tools`
//! - [`doctor`] — `/api/doctor` (16-check master) and its
//!   provider-reachability / security-audit helpers
//! - [`watchdog`] — `/api/watchdog/*`
//!
//! Shared by sub-modules: the wire-format types [`CheckResult`] +
//! [`CheckStatus`], consumed by the doctor + setup-requirements
//! endpoints to pin a stable JSON contract for the UI dashboard.

use axum::{
    Router,
    extract::State,
    middleware as axum_mw,
    response::Json,
    routing::{get, post},
};
use serde::Serialize;
use serde_json::Value;

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, ChannelBus, InfraServices, StatusMonitor};

mod audit;
mod doctor;
mod setup;
mod status;
mod usage;
mod watchdog;
mod watchdog_endpoint;

pub(crate) fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/setup/status", get(setup::api_setup_status))
        .route("/api/setup/requirements", get(setup::api_setup_requirements))
        .merge(
            Router::new()
                .route("/api/setup/complete", post(setup::api_setup_complete))
                .layer(axum_mw::from_fn_with_state(state, setup::setup_guard_middleware))
        )
        .route("/api/status", get(status::api_status))
        .route("/api/stats", get(status::api_stats))
        .route("/api/usage", get(usage::api_usage))
        .route("/api/usage/daily", get(usage::api_usage_daily))
        .route("/api/tools/health", get(usage::api_tools_health))
        .route("/api/doctor", get(doctor::api_doctor))
        .route("/api/health/dashboard", get(api_health_dashboard))
        .route("/api/audit", get(audit::api_audit_events))
        .route("/api/watchdog/status", get(watchdog::api_watchdog_status))
        .route("/api/watchdog/config", get(watchdog::api_watchdog_config).put(watchdog::api_watchdog_config_update))
        .route("/api/watchdog/settings", get(watchdog::api_watchdog_settings).put(watchdog::api_watchdog_settings_update))
        .route("/api/watchdog/restart/{name}", post(watchdog::api_watchdog_restart_check))
        .route("/api/watchdog/agent-activity", get(watchdog_endpoint::api_watchdog_agent_activity))
}

// ── Doctor check types ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CheckStatus {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct CheckResult {
    pub status: CheckStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl CheckResult {
    pub(super) fn ok(message: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            status: CheckStatus::Ok,
            message: message.into(),
            latency_ms: Some(latency_ms),
            fix_hint: None,
            details: None,
        }
    }
    pub(super) fn warn(message: impl Into<String>, latency_ms: u64, fix_hint: Option<String>) -> Self {
        Self {
            status: CheckStatus::Warn,
            message: message.into(),
            latency_ms: Some(latency_ms),
            fix_hint,
            details: None,
        }
    }
    pub(super) fn error(message: impl Into<String>, latency_ms: u64, fix_hint: Option<String>) -> Self {
        Self {
            status: CheckStatus::Error,
            message: message.into(),
            latency_ms: Some(latency_ms),
            fix_hint,
            details: None,
        }
    }
    pub(super) fn timeout(check_name: &str) -> Self {
        Self {
            status: CheckStatus::Error,
            message: format!("{check_name} check timed out after 3s"),
            latency_ms: Some(3000),
            fix_hint: None,
            details: None,
        }
    }
}

/// GET /api/health/dashboard — Phase 62 RES-02 + Phase 65 OBS-05 resilience
/// metrics.
///
/// Response shape:
/// ```json
/// {
///   "version": "0.19.0",
///   "sse_events_dropped_total": { "<agent>": { "<event_type>": <u64> } },
///   "csp_violations": { "<directive>": <u64> },
///   "csp_violations_overflow": <u64>,
///   "active_agents": <u64>,
///   "sse_streams": <u64>,
///   "approval_waiters": <u64>,
///   "auth_rate_limiter_size": <u64>,
///   "request_rate_limiter_size": <u64>,
///   "stream_registry_size": <u64>,
///   "db_pool_total": <u64>,
///   "db_pool_idle": <u64>,
///   "memory_worker_heartbeat_age_secs": <i64>,
///   "session_timeline_table_size_bytes": <u64>,
///   "uptime_secs": <u64>,
///   "cache_read_tokens_24h": <i64>,
///   "cache_creation_tokens_24h": <i64>,
///   "cache_read_tokens_7d": <i64>,
///   "cache_creation_tokens_7d": <i64>
/// }
/// ```
///
/// Body construction is delegated to
/// `crate::metrics::build_dashboard_body_with_snapshot` so integration tests
/// can pin both the nested `sse_events_dropped_total` contract (Phase 62)
/// AND the ≥10-field extension contract (Phase 65 OBS-05) without
/// extracting a handler-state harness. Clients MUST treat unknown fields
/// as opaque.
pub(crate) async fn api_health_dashboard(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(channels): State<ChannelBus>,
    State(status): State<StatusMonitor>,
) -> Json<Value> {
    use crate::metrics::{DashboardSnapshot, build_dashboard_body_with_snapshot};

    // ── Cluster reads (all cheap, in-process) ────────────────────────────
    let active_agents = agents.map.read().await.len() as u64;
    let approval_waiters = agents.approval_waiters_size().await;
    let sse_streams = channels.stream_registry.snapshot_size().await;
    let (auth_rate_limiter_size, request_rate_limiter_size) =
        crate::gateway::middleware::rate_limiter_sizes().await;

    // sqlx::PgPool live counters — O(1) atomic reads.
    let db_pool_total = infra.db.size() as u64;
    let db_pool_idle = infra.db.num_idle() as u64;

    // ── DB-backed reads (bounded; cheap — both queries read metadata or a
    //    single aggregate, never scan data pages) ─────────────────────────
    let session_timeline_table_size_bytes: u64 = sqlx::query_scalar::<_, i64>(
        "SELECT pg_total_relation_size('session_timeline')",
    )
    .fetch_one(&infra.db)
    .await
    .unwrap_or(0)
    .max(0) as u64;

    // Memory-worker heartbeat: age in seconds since the worker last touched a task.
    // GREATEST(completed_at, started_at) captures both "finished a task" and
    // "currently working on one" — whichever is newer. memory_tasks schema uses
    // 'done' for successful completion (not 'complete'); include all statuses
    // the worker may have advanced.
    // If the table is empty or unreachable, emit -1 (unknown) so dashboards
    // can display "n/a" rather than mis-interpret 0 as "just ran".
    let memory_worker_heartbeat_age_secs: i64 = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT EXTRACT(EPOCH FROM (NOW() - GREATEST(MAX(completed_at), MAX(started_at))))::BIGINT \
         FROM memory_tasks \
         WHERE status IN ('processing', 'done', 'failed')",
    )
    .fetch_one(&infra.db)
    .await
    .ok()
    .flatten()
    .unwrap_or(-1);

    // CACHE-03: aggregate cache-token usage from usage_log.
    // `unwrap_or_default()` degrades gracefully on DB error — dashboard
    // continues to render with zeros rather than failing the request.
    // Same posture as `session_timeline_table_size_bytes` and the
    // `memory_worker_heartbeat_age_secs` reads above.
    let cache_tokens = opex_db::usage::cache_metrics(&infra.db)
        .await
        .unwrap_or_default();

    let snap = DashboardSnapshot {
        active_agents,
        sse_streams,
        approval_waiters,
        auth_rate_limiter_size,
        request_rate_limiter_size,
        // `stream_registry_size` is an alias of `sse_streams` so clients can
        // pick whichever label fits; both emitted for clarity.
        stream_registry_size: sse_streams,
        db_pool_total,
        db_pool_idle,
        memory_worker_heartbeat_age_secs,
        session_timeline_table_size_bytes,
        uptime_secs: status.started_at.elapsed().as_secs(),
        // CACHE-03: prompt-cache token aggregates (24h + 7d windows).
        cache_read_tokens_24h: cache_tokens.cache_read_tokens_24h,
        cache_creation_tokens_24h: cache_tokens.cache_creation_tokens_24h,
        cache_read_tokens_7d: cache_tokens.cache_read_tokens_7d,
        cache_creation_tokens_7d: cache_tokens.cache_creation_tokens_7d,
    };

    Json(build_dashboard_body_with_snapshot(&infra.metrics, &snap))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CheckResult constructor tests ───────────────────────────────────────

    #[test]
    fn check_result_ok_carries_message_and_latency() {
        let r = CheckResult::ok("db reachable", 12);
        assert!(matches!(r.status, CheckStatus::Ok));
        assert_eq!(r.message, "db reachable");
        assert_eq!(r.latency_ms, Some(12));
        assert!(r.fix_hint.is_none());
        assert!(r.details.is_none());
    }

    #[test]
    fn check_result_warn_carries_fix_hint() {
        let r = CheckResult::warn("slow", 850, Some("check network".into()));
        assert!(matches!(r.status, CheckStatus::Warn));
        assert_eq!(r.fix_hint.as_deref(), Some("check network"));
    }

    #[test]
    fn check_result_error_carries_fix_hint() {
        let r = CheckResult::error("unreachable", 3000, Some("start service".into()));
        assert!(matches!(r.status, CheckStatus::Error));
        assert_eq!(r.fix_hint.as_deref(), Some("start service"));
    }

    #[test]
    fn check_result_timeout_uses_3000ms_and_includes_check_name() {
        let r = CheckResult::timeout("toolgate");
        assert!(matches!(r.status, CheckStatus::Error));
        assert_eq!(r.latency_ms, Some(3000));
        assert!(r.message.contains("toolgate"));
        assert!(r.message.contains("3s"));
    }

    // ── CheckStatus serialization ───────────────────────────────────────────

    #[test]
    fn check_status_serializes_to_lowercase_string() {
        assert_eq!(serde_json::to_string(&CheckStatus::Ok).unwrap(), "\"ok\"");
        assert_eq!(serde_json::to_string(&CheckStatus::Warn).unwrap(), "\"warn\"");
        assert_eq!(serde_json::to_string(&CheckStatus::Error).unwrap(), "\"error\"");
    }

    #[test]
    fn check_result_serializes_optional_fields_with_skip_if_none() {
        let r = CheckResult::ok("ok", 1);
        let json = serde_json::to_value(&r).unwrap();
        // When fix_hint and details are None, they MUST NOT appear in the
        // JSON — clients that strictly check for `if (resp.fix_hint)`
        // would otherwise see `null` and treat that as a missing string.
        assert!(json.get("fix_hint").is_none());
        assert!(json.get("details").is_none());
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("ok"));
    }
}
