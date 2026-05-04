use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    middleware as axum_mw,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::{
    AgentCore, AuthServices, ChannelBus, ConfigServices, InfraServices, StatusMonitor,
};
use crate::agent::cli_backend::CLI_PRESETS;

pub(crate) fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/setup/status", get(api_setup_status))
        .route("/api/setup/requirements", get(api_setup_requirements))
        .merge(
            Router::new()
                .route("/api/setup/complete", post(api_setup_complete))
                .layer(axum_mw::from_fn_with_state(state, setup_guard_middleware))
        )
        .route("/api/status", get(api_status))
        .route("/api/stats", get(api_stats))
        .route("/api/usage", get(api_usage))
        .route("/api/usage/daily", get(api_usage_daily))
        .route("/api/usage/sessions", get(api_usage_sessions))
        .route("/api/doctor", get(api_doctor))
        .route("/api/health/dashboard", get(api_health_dashboard))
        .route("/api/audit", get(api_audit_events))
        .route("/api/audit/tools", get(api_tool_audit))
        .route("/api/watchdog/status", get(api_watchdog_status))
        .route("/api/watchdog/config", get(api_watchdog_config).put(api_watchdog_config_update))
        .route("/api/watchdog/settings", get(api_watchdog_settings).put(api_watchdog_settings_update))
        .route("/api/watchdog/restart/{name}", post(api_watchdog_restart_check))
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
///   "session_events_table_size_bytes": <u64>,
///   "uptime_secs": <u64>
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
    let session_events_table_size_bytes: u64 = sqlx::query_scalar::<_, i64>(
        "SELECT pg_total_relation_size('session_events')",
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
        session_events_table_size_bytes,
        uptime_secs: status.started_at.elapsed().as_secs(),
    };

    Json(build_dashboard_body_with_snapshot(&infra.metrics, &snap))
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
    fn ok(message: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            status: CheckStatus::Ok,
            message: message.into(),
            latency_ms: Some(latency_ms),
            fix_hint: None,
            details: None,
        }
    }
    fn warn(message: impl Into<String>, latency_ms: u64, fix_hint: Option<String>) -> Self {
        Self {
            status: CheckStatus::Warn,
            message: message.into(),
            latency_ms: Some(latency_ms),
            fix_hint,
            details: None,
        }
    }
    fn error(message: impl Into<String>, latency_ms: u64, fix_hint: Option<String>) -> Self {
        Self {
            status: CheckStatus::Error,
            message: message.into(),
            latency_ms: Some(latency_ms),
            fix_hint,
            details: None,
        }
    }
    fn timeout(check_name: &str) -> Self {
        Self {
            status: CheckStatus::Error,
            message: format!("{check_name} check timed out after 3s"),
            latency_ms: Some(3000),
            fix_hint: None,
            details: None,
        }
    }
}

pub(crate) async fn api_setup_status(State(infra): State<InfraServices>) -> Json<Value> {
    let complete: bool = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM system_flags WHERE key = 'setup_complete'"
    )
    .fetch_optional(&infra.db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_bool())
    .unwrap_or(false);

    Json(json!({ "needs_setup": !complete }))
}

/// POST /api/setup/complete — mark setup as done; guarded by `setup_guard_middleware`
pub(crate) async fn api_setup_complete(State(infra): State<InfraServices>) -> impl IntoResponse {
    let result = sqlx::query(
        "INSERT INTO system_flags (key, value, updated_at)
         VALUES ('setup_complete', 'true'::jsonb, NOW())
         ON CONFLICT (key) DO UPDATE SET value = 'true'::jsonb, updated_at = NOW()"
    )
    .execute(&infra.db)
    .await;

    match result {
        Ok(_) => Json(json!({"ok": true, "message": "setup marked as complete"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()}))
        ).into_response(),
    }
}

/// Check whether a CLI tool is installed and get its version/path.
async fn check_cli_tool(name: &str, command: &str) -> serde_json::Value {
    let which_cmd = if cfg!(target_os = "windows") {
        "where.exe"
    } else {
        "which"
    };

    let which_result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new(which_cmd)
            .arg(command)
            .output(),
    )
    .await;

    let path = match which_result {
        Ok(Ok(out)) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let first_line = stdout.lines().next().unwrap_or("").trim().to_string();
            if first_line.is_empty() {
                return json!({ "name": name, "status": "not_found" });
            }
            first_line
        }
        _ => return json!({ "name": name, "status": "not_found" }),
    };

    // Try to get version
    let version = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new(command)
            .arg("--version")
            .output(),
    )
    .await
    {
        Ok(Ok(out)) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let raw = stdout.lines().next().unwrap_or("").trim().to_string();
            // Strip common prefixes like "gemini version 0.36.0" → "0.36.0"
            let version = raw
                .rsplit(' ')
                .next()
                .unwrap_or(&raw)
                .trim_start_matches('v')
                .to_string();
            if version.is_empty() { None } else { Some(version) }
        }
        _ => None,
    };

    let mut result = json!({ "name": name, "status": "ok", "path": path });
    if let Some(v) = version {
        result["version"] = json!(v);
    }
    result
}

/// GET /api/setup/requirements — pre-flight system requirements check for the setup wizard.
/// Returns docker, postgresql, and `disk_space` check results. No auth required.
pub(crate) async fn api_setup_requirements(State(infra): State<InfraServices>) -> Json<Value> {
    // ── Docker check ──────────────────────────────────────────────────────────
    let docker_fut = async {
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(4),
            tokio::process::Command::new("docker")
                .args(["info", "--format", "{{.ServerVersion}}"])
                .output(),
        )
        .await;
        let ms = start.elapsed().as_millis() as u64;
        match result {
            Err(_) => CheckResult::timeout("docker"),
            Ok(Err(_)) => CheckResult::warn(
                "docker binary not found",
                ms,
                Some("install Docker for MCP and sandbox features".into()),
            ),
            Ok(Ok(out)) if out.status.success() => {
                let version = String::from_utf8_lossy(&out.stdout);
                let version = version.trim();
                if version.is_empty() {
                    CheckResult::error(
                        "docker not running or not installed",
                        ms,
                        Some("install Docker and ensure the daemon is running".into()),
                    )
                } else {
                    CheckResult::ok(format!("docker {version}"), ms)
                }
            }
            Ok(Ok(_)) => CheckResult::error(
                "docker not running or not installed",
                ms,
                Some("install Docker and ensure the daemon is running".into()),
            ),
        }
    };

    // ── PostgreSQL check ──────────────────────────────────────────────────────
    let pg_db = infra.db.clone();
    let pg_fut = async {
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            sqlx::query("SELECT 1").execute(&pg_db),
        )
        .await;
        let ms = start.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(_)) => CheckResult::ok("postgresql reachable", ms),
            Ok(Err(_)) => CheckResult::error(
                "postgresql unreachable",
                ms,
                Some("check DATABASE_URL and PostgreSQL service".into()),
            ),
            Err(_) => CheckResult::timeout("postgresql"),
        }
    };

    // ── CLI tool detection ─────────────────────────────────────────────────
    let cli_fut = async {
        let futs: Vec<_> = CLI_PRESETS
            .iter()
            .map(|p| check_cli_tool(p.id, p.command))
            .collect();
        futures_util::future::join_all(futs).await
    };

    let (docker_check, postgresql_check, cli_tools) = tokio::join!(docker_fut, pg_fut, cli_fut);

    // ── Disk space check (Linux only) ─────────────────────────────────────────
    #[cfg(target_os = "linux")]
    let disk_check = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            let start = std::time::Instant::now();
            let out = tokio::process::Command::new("df")
                .args(["-k", "--output=avail", "."])
                .output()
                .await;
            let ms = start.elapsed().as_millis() as u64;
            match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let avail_kb: i64 = text
                        .lines()
                        .nth(1)
                        .and_then(|l| l.trim().parse().ok())
                        .unwrap_or(i64::MAX);
                    let avail_mb = avail_kb / 1024;
                    if avail_kb < 102_400 {
                        CheckResult::error(
                            format!("{} MB disk free (critical)", avail_mb),
                            ms,
                            Some("free disk space immediately — system may become unstable".into()),
                        )
                    } else if avail_kb < 512_000 {
                        CheckResult::warn(
                            format!("{} MB disk free (low)", avail_mb),
                            ms,
                            Some("consider freeing disk space or expanding storage".into()),
                        )
                    } else {
                        CheckResult::ok(format!("{} MB disk free", avail_mb), ms)
                    }
                }
                Ok(o) => CheckResult::warn(
                    format!(
                        "df exited with error: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                    ms,
                    None,
                ),
                Err(e) => CheckResult::warn(format!("disk check failed: {}", e), ms, None),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("disk"));

    #[cfg(not(target_os = "linux"))]
    let disk_check = CheckResult {
        status: CheckStatus::Ok,
        message: "disk check not available on this platform".into(),
        latency_ms: None,
        fix_hint: None,
        details: None,
    };

    // ── Compute overall ok ────────────────────────────────────────────────────
    let all_checks = [&docker_check, &postgresql_check, &disk_check];
    let all_ok = all_checks
        .iter()
        .all(|c| !matches!(c.status, CheckStatus::Error));

    Json(json!({
        "ok": all_ok,
        "checks": {
            "docker": docker_check,
            "postgresql": postgresql_check,
            "disk_space": disk_check,
        },
        "cli_tools": cli_tools,
    }))
}

/// Axum middleware: returns 403 when `system_flags.setup_complete` = true.
/// Wraps POST /api/setup/complete to prevent re-entry after first setup.
pub(crate) async fn setup_guard_middleware(
    State(infra): State<InfraServices>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let complete: bool = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM system_flags WHERE key = 'setup_complete'"
    )
    .fetch_optional(&infra.db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_bool())
    .unwrap_or(false);

    if complete {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "setup already complete"}))
        ).into_response();
    }
    next.run(req).await
}

pub(crate) async fn api_status(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
) -> Json<Value> {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&infra.db)
        .await
        .is_ok();

    let uptime_secs = status.started_at.elapsed().as_secs();

    let memory_chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    let scheduled_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_jobs WHERE enabled = true")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    let active_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE last_message_at > now() - interval '4 hours'",
    )
    .fetch_one(&infra.db)
    .await
    .unwrap_or(0);

    let config = cfg_svc.shared_config.read().await;

    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": uptime_secs,
        "db": db_ok,
        "listen": config.gateway.listen,
        "agents": agents.agent_names().await,
        "memory_chunks": memory_chunks,
        "scheduled_jobs": scheduled_jobs,
        "active_sessions": active_sessions,
        "tools_registered": agents.tools.len().await + {
            // Count YAML tool files without parsing them (avoid filesystem overhead per request)
            let yaml_count = match tokio::fs::read_dir("workspace/tools").await {
                Ok(mut dir) => {
                    let mut count = 0u64;
                    while let Ok(Some(entry)) = dir.next_entry().await {
                        if entry.path().extension().is_some_and(|e| e == "yaml" || e == "yml") {
                            count += 1;
                        }
                    }
                    count
                }
                Err(_) => 0,
            };
            yaml_count as usize
        },
    }))
}

pub(crate) async fn api_stats(State(infra): State<InfraServices>) -> Json<Value> {
    let messages_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE created_at > CURRENT_DATE",
    )
    .fetch_one(&infra.db)
    .await
    .unwrap_or(0);

    let sessions_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE started_at > CURRENT_DATE",
    )
    .fetch_one(&infra.db)
    .await
    .unwrap_or(0);

    let total_messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    let total_sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    #[allow(clippy::type_complexity)]
    let recent_sessions: Vec<(uuid::Uuid, String, String, chrono::DateTime<chrono::Utc>, Option<String>)> =
        sqlx::query_as(
            "SELECT id, agent_id, channel, last_message_at, title \
             FROM sessions \
             WHERE last_message_at > NOW() - INTERVAL '24 hours' \
             ORDER BY last_message_at DESC LIMIT 10",
        )
        .fetch_all(&infra.db)
        .await
        .unwrap_or_default();

    let recent: Vec<Value> = recent_sessions.iter().map(|(id, agent, channel, ts, title)| {
        json!({ "id": id, "agent_id": agent, "channel": channel, "last_message_at": ts, "title": title })
    }).collect();

    Json(json!({
        "messages_today": messages_today,
        "sessions_today": sessions_today,
        "total_messages": total_messages,
        "total_sessions": total_sessions,
        "recent_sessions": recent,
    }))
}

// ── Usage API ──

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

// ── Audit API ──

#[derive(Deserialize)]
pub(crate) struct AuditQuery {
    agent: Option<String>,
    event_type: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(crate) async fn api_audit_events(
    State(infra): State<InfraServices>,
    Query(q): Query<AuditQuery>,
) -> Json<Value> {
    let limit = q.limit.unwrap_or(100).min(500);
    let offset = q.offset.unwrap_or(0);
    match crate::db::audit::query_events(
        &infra.db,
        q.agent.as_deref(),
        q.event_type.as_deref(),
        limit,
        offset,
    ).await {
        Ok(events) => Json(json!({"ok": true, "events": events, "limit": limit, "offset": offset})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

// ── Tool Audit Log API ──

#[derive(Deserialize)]
pub(crate) struct ToolAuditQuery {
    agent: Option<String>,
    tool: Option<String>,
    days: Option<u32>,
    limit: Option<i64>,
}

pub(crate) async fn api_tool_audit(
    State(infra): State<InfraServices>,
    Query(q): Query<ToolAuditQuery>,
) -> Json<Value> {
    let days = q.days.unwrap_or(7);
    let limit = q.limit.unwrap_or(100).min(500);
    match crate::db::tool_audit::query_tool_audit(
        &infra.db,
        q.agent.as_deref(),
        q.tool.as_deref(),
        days,
        limit,
    ).await {
        Ok(entries) => Json(json!({"ok": true, "entries": entries, "days": days, "limit": limit})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

// ── Provider reachability check ───────────────────────────────────────────────

async fn check_provider_reachability(infra: &InfraServices, auth: &AuthServices) -> CheckResult {
    let start = std::time::Instant::now();
    let providers = match crate::db::providers::list_providers(&infra.db).await {
        Ok(p) => p,
        Err(e) => return CheckResult::error(
            format!("failed to list providers: {e}"),
            start.elapsed().as_millis() as u64,
            Some("check database connectivity".into()),
        ),
    };

    let enabled: Vec<_> = providers.into_iter().filter(|p| p.enabled).collect();
    if enabled.is_empty() {
        return CheckResult {
            status: CheckStatus::Ok,
            message: "no providers configured".into(),
            latency_ms: Some(start.elapsed().as_millis() as u64),
            fix_hint: Some("add a provider in the Providers page".into()),
            details: None,
        };
    }

    let http = crate::net::ssrf::ssrf_http_client(std::time::Duration::from_secs(3));

    let mut results = serde_json::Map::new();
    let mut any_error = false;
    let mut any_warn = false;

    for p in &enabled {
        let provider_start = std::time::Instant::now();

        let base_url = p.base_url.as_deref().unwrap_or("");
        let is_local = base_url.starts_with("http://localhost") || base_url.starts_with("http://127.");

        let has_cred = is_local || auth.secrets.get_scoped(
            crate::agent::providers::PROVIDER_CREDENTIALS,
            &p.id.to_string(),
        ).await.is_some();

        let (status, message, fix_hint) = if base_url.is_empty() {
            any_warn = true;
            ("warn", format!("{} has no base_url configured", p.name),
             Some("set base_url in Providers page".to_string()))
        } else if !has_cred {
            any_warn = true;
            ("warn", format!("{} has no API credential stored", p.name),
             Some("add API key in Providers page".to_string()))
        } else {
            let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
            match http.get(&url).send().await {
                Ok(r) if r.status().is_success()
                    || r.status().as_u16() == 401
                    || r.status().as_u16() == 403
                    || (!is_local && (r.status().as_u16() == 404 || r.status().as_u16() == 405)) => {
                    // Server responded — reachable. External APIs may not support GET /v1/models.
                    ("ok", format!("{} reachable", p.name), None)
                }
                Ok(r) => {
                    any_warn = true;
                    ("warn", format!("{} returned HTTP {}", p.name, r.status()),
                     Some("check provider base_url in Providers page".to_string()))
                }
                Err(_) => {
                    any_error = true;
                    ("error", format!("{} unreachable", p.name),
                     Some("check provider base_url and network connectivity".to_string()))
                }
            }
        };

        let ms = provider_start.elapsed().as_millis() as u64;
        results.insert(p.name.clone(), serde_json::json!({
            "status": status,
            "message": message,
            "latency_ms": ms,
            "fix_hint": fix_hint,
            "category": p.category,
        }));
    }

    let overall_status = if any_error { CheckStatus::Error }
        else if any_warn { CheckStatus::Warn }
        else { CheckStatus::Ok };
    let ok_count = results.values()
        .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("ok"))
        .count();

    CheckResult {
        status: overall_status,
        message: format!("{}/{} providers reachable", ok_count, enabled.len()),
        latency_ms: Some(start.elapsed().as_millis() as u64),
        fix_hint: None,
        details: Some(serde_json::Value::Object(results)),
    }
}

// ── Security audit check ─────────────────────────────────────────────────────

async fn check_security_audit(_infra: &InfraServices) -> CheckResult {
    use regex::Regex;

    let start = std::time::Instant::now();

    // Credential patterns
    let patterns: &[(&'static str, &'static str)] = &[
        (r"sk-[a-zA-Z0-9]{40,}", "OpenAI key"),
        (r"ghp_[a-zA-Z0-9]{36}", "GitHub token"),
        (r"AIza[0-9A-Za-z\-_]{35}", "Google API key"),
        (r#"[Aa][Pp][Ii][_-]?[Kk][Ee][Yy]\s*[:=]\s*['"]?[a-zA-Z0-9]{20,}"#, "generic API key"),
    ];

    // Walk workspace/ (skip uploads/)
    let workspace_dir = std::path::Path::new("workspace");
    let mut credential_findings: Vec<serde_json::Value> = Vec::new();
    let mut files_scanned = 0usize;

    fn walk_dir_sync(
        dir: &std::path::Path,
        compiled: &[(regex::Regex, &str)],
        findings: &mut Vec<serde_json::Value>,
        count: &mut usize,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "uploads") { continue; }
                if *count >= 1000 { break; }
                walk_dir_sync(&path, compiled, findings, count);
            } else {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !["md", "yaml", "yml", "txt"].contains(&ext) { continue; }
                *count += 1;
                if *count > 1000 { break; }
                let Ok(content) = std::fs::read(&path) else { continue };
                if content.len() > 100_000 { continue; }
                let text = String::from_utf8_lossy(&content);
                for (re, pattern_name) in compiled {
                    if re.is_match(&text) {
                        findings.push(serde_json::json!({
                            "file": path.display().to_string(),
                            "pattern": pattern_name,
                        }));
                        break; // one finding per file
                    }
                }
            }
        }
    }

    // Run the blocking filesystem walk off the async thread to avoid blocking the executor
    if workspace_dir.exists() {
        let workspace_dir_owned = workspace_dir.to_path_buf();
        let compiled_owned: Vec<(Regex, &'static str)> = patterns.iter()
            .filter_map(|(pat, name)| Regex::new(pat).ok().map(|r| (r, *name)))
            .collect();
        let (findings, scanned) = tokio::task::spawn_blocking(move || {
            let mut findings: Vec<serde_json::Value> = Vec::new();
            let mut count = 0usize;
            walk_dir_sync(&workspace_dir_owned, &compiled_owned, &mut findings, &mut count);
            (findings, count)
        })
        .await
        .unwrap_or_default();
        credential_findings = findings;
        files_scanned = scanned;
    }

    // Tool deny-list audit
    let config_dir = std::path::Path::new("config/agents");
    let mut deny_list_issues: Vec<serde_json::Value> = Vec::new();
    let dangerous_tools = ["code_exec", "process_start"];

    if let Ok(entries) = std::fs::read_dir(config_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") { continue; }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(val) = toml::from_str::<toml::Value>(&text) else { continue };

            let is_base = val.get("agent")
                .and_then(|a| a.get("base"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            if is_base { continue; } // base agents are intentionally unrestricted

            let deny_list = val.get("agent")
                .and_then(|a| a.get("tools"))
                .and_then(|t| t.get("deny"))
                .and_then(|d| d.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                .unwrap_or_default();

            let agent_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");

            for tool in &dangerous_tools {
                if !deny_list.contains(tool) {
                    deny_list_issues.push(serde_json::json!({
                        "agent": agent_name,
                        "tool": tool,
                        "issue": "dangerous tool not in deny list",
                    }));
                }
            }
        }
    }

    // Suppress unused field warning — _infra is passed for future extensibility

    let ms = start.elapsed().as_millis() as u64;
    let has_cred_leaks = !credential_findings.is_empty();
    let has_deny_issues = !deny_list_issues.is_empty();

    let status = if has_cred_leaks {
        CheckStatus::Error
    } else if has_deny_issues {
        CheckStatus::Warn
    } else {
        CheckStatus::Ok
    };

    let message = match (has_cred_leaks, has_deny_issues) {
        (true, _) => format!("{} credential leak(s) found in workspace files", credential_findings.len()),
        (false, true) => format!("{} agent(s) missing tool deny-list entries", deny_list_issues.len()),
        (false, false) => format!("no issues found ({files_scanned} files scanned)"),
    };

    CheckResult {
        status,
        message,
        latency_ms: Some(ms),
        fix_hint: if has_cred_leaks {
            Some("move API keys to secrets vault via Secrets page; remove from workspace files".into())
        } else if has_deny_issues {
            Some("add dangerous tools to deny list in agent config".into())
        } else {
            None
        },
        details: Some(serde_json::json!({
            "files_scanned": files_scanned,
            "credential_findings": credential_findings,
            "deny_list_issues": deny_list_issues,
        })),
    }
}

// ── Doctor / Health-check API ──

pub(crate) async fn api_doctor(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
) -> Json<Value> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let config = cfg_svc.shared_config.read().await;
    let toolgate_url = config.toolgate_url.clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    let br_base = std::env::var("BROWSER_RENDERER_URL")
        .unwrap_or_else(|_| "http://localhost:9020".to_string());
    drop(config);

    // ── 1. Database check ──────────────────────────────────────────────────
    let db_clone = infra.db.clone();
    let database_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let ok = sqlx::query("SELECT 1").execute(&db_clone).await.is_ok();
            let ms = start.elapsed().as_millis() as u64;
            if ok {
                CheckResult::ok("database reachable", ms)
            } else {
                CheckResult::error(
                    "database unreachable",
                    ms,
                    Some("check DATABASE_URL and PostgreSQL service".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("database"));

    // ── 2. Toolgate check ─────────────────────────────────────────────────
    let tg_http = http.clone();
    let tg_url = toolgate_url.clone();
    let toolgate_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let result = tg_http.get(format!("{tg_url}/health")).send().await;
            let ms = start.elapsed().as_millis() as u64;
            match result {
                Ok(r) if r.status().is_success() => {
                    let body: Value = r.json().await.unwrap_or(Value::Null);
                    let providers = body.get("active_providers").cloned().unwrap_or(Value::Null);
                    let mut cr = CheckResult::ok("toolgate reachable", ms);
                    cr.details = Some(json!({"providers": providers}));
                    cr
                }
                _ => CheckResult::error(
                    "toolgate unreachable",
                    ms,
                    Some("check toolgate process is running".into()),
                ),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("toolgate"));

    // ── 3. Browser renderer check ─────────────────────────────────────────
    let br_http = http.clone();
    let br_url = br_base.clone();
    let browser_renderer_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let ok = br_http.get(format!("{br_url}/health")).send().await
                .map(|r| r.status().is_success()).unwrap_or(false);
            let ms = start.elapsed().as_millis() as u64;
            if ok {
                CheckResult::ok("browser renderer reachable", ms)
            } else {
                CheckResult::warn(
                    "browser renderer not reachable",
                    ms,
                    Some("start browser-renderer container if screenshot tools are needed".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("browser_renderer"));

    // ── 4. SearXNG check ──────────────────────────────────────────────────
    let sx_http = http.clone();
    let searxng_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let ok = sx_http.get("http://localhost:8080/healthz").send().await
                .map(|r| r.status().is_success()).unwrap_or(false);
            let ms = start.elapsed().as_millis() as u64;
            if ok {
                CheckResult::ok("searxng reachable", ms)
            } else {
                CheckResult::warn(
                    "searxng not reachable",
                    ms,
                    Some("start searxng container if web search tools are needed".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("searxng"));

    // ── 5. Secrets check ──────────────────────────────────────────────────
    let mut missing_critical: Vec<String> = Vec::new();
    if let Ok(providers) = crate::db::providers::list_providers_by_type(&infra.db, "text").await {
        for p in &providers {
            let has_key = auth.secrets.get_scoped(
                crate::agent::providers::PROVIDER_CREDENTIALS,
                &p.id.to_string(),
            ).await.is_some();
            if !has_key {
                missing_critical.push(format!("LLM:{}", p.name));
            }
        }
    }
    if let Ok(channels) = sqlx::query_as::<_, (sqlx::types::Uuid, String, String)>(
        "SELECT id, agent_name, channel_type FROM agent_channels WHERE status != 'deleted'"
    ).fetch_all(&infra.db).await {
        for (id, agent, ch_type) in &channels {
            if auth.secrets.get_scoped("CHANNEL_CREDENTIALS", &id.to_string()).await.is_none() {
                missing_critical.push(format!("Channel:{agent}:{ch_type}"));
            }
        }
    }
    let secrets_count = auth.secrets.list().await.map(|v| v.len()).unwrap_or(0);
    let secrets_check = {
        let mut cr = if missing_critical.is_empty() {
            CheckResult::ok(format!("{secrets_count} secrets configured"), 0)
        } else {
            CheckResult::warn(
                format!("{} missing credential(s)", missing_critical.len()),
                0,
                Some("add missing credentials via the Secrets page or vault API".into()),
            )
        };
        cr.details = Some(json!({
            "count": secrets_count,
            "missing_critical": missing_critical,
        }));
        cr
    };

    // ── 6. Channels health check ───────────────────────────────────────────
    let ch_http = http.clone();
    let channels_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let ok = ch_http.get("http://localhost:3100/health").send().await
                .map(|r| r.status().is_success()).unwrap_or(false);
            let ms = start.elapsed().as_millis() as u64;
            if ok {
                CheckResult::ok("channels adapter reachable", ms)
            } else {
                CheckResult::warn(
                    "channels adapter not reachable",
                    ms,
                    Some("check channels process is running".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("channels"));

    // ── 7. Agent statuses ─────────────────────────────────────────────────
    let agents_map = agents.map.read().await;
    let agent_count = agents_map.len();
    let mut agents_details = serde_json::Map::new();
    for (name, _handle) in agents_map.iter() {
        agents_details.insert(name.clone(), json!({"status": "ok"}));
    }
    drop(agents_map);
    let agents_check = {
        let mut cr = CheckResult::ok(format!("{agent_count} agent(s) loaded"), 0);
        cr.details = Some(Value::Object(agents_details));
        cr
    };

    // ── 8. Tool health ────────────────────────────────────────────────────
    let degraded_tools = crate::db::tool_quality::get_degraded_tools(&infra.db)
        .await.unwrap_or_default();
    let degraded_count = degraded_tools.len();
    let tool_health_check = {
        let mut cr = if degraded_count == 0 {
            CheckResult::ok("all tools healthy", 0)
        } else {
            CheckResult::warn(
                format!("{degraded_count} degraded tool(s)"),
                0,
                Some("review tool audit log for repeated failures".into()),
            )
        };
        cr.details = Some(json!({
            "degraded": degraded_tools,
            "degraded_count": degraded_count,
        }));
        cr
    };

    // ── 9. DB migration lag check ─────────────────────────────────────────
    let mig_db = infra.db.clone();
    let migrations_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            // applied: rows in sqlx tracking table
            let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
                .fetch_one(&mig_db)
                .await
                .unwrap_or(0);
            // total: load migration files from disk at runtime (same path used by main.rs)
            let total = match sqlx::migrate::Migrator::new(std::path::Path::new("migrations")).await {
                Ok(m) => m.migrations.len() as i64,
                Err(_) => applied, // can't determine total — assume up to date
            };
            let pending = (total - applied).max(0);
            let ms = start.elapsed().as_millis() as u64;
            if pending > 0 {
                CheckResult::warn(
                    format!("{pending} migration(s) pending"),
                    ms,
                    Some("restart the service to apply pending migrations".into()),
                )
            } else {
                CheckResult::ok(format!("all {total} migrations applied"), ms)
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("migrations"));

    // ── 10. pgvector extension check ──────────────────────────────────────
    let pg_db = infra.db.clone();
    let pgvector_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let present: bool = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'vector')",
            )
            .fetch_one(&pg_db)
            .await
            .unwrap_or(false);
            let ms = start.elapsed().as_millis() as u64;
            if present {
                CheckResult::ok("pgvector extension installed", ms)
            } else {
                CheckResult::error(
                    "pgvector extension missing",
                    ms,
                    Some("run: CREATE EXTENSION IF NOT EXISTS vector; in your database".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("pgvector"));

    // ── 11. Memory worker check (Linux only) ──────────────────────────────
    #[cfg(target_os = "linux")]
    let memory_worker_check = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            let start = std::time::Instant::now();
            let out = tokio::process::Command::new("systemctl")
                .args(["--user", "is-active", "hydeclaw-memory-worker"])
                .output()
                .await;
            let ms = start.elapsed().as_millis() as u64;
            match out {
                Ok(o) if o.status.success() => CheckResult::ok("memory worker active", ms),
                Ok(_) => CheckResult::warn(
                    "memory worker not active",
                    ms,
                    Some("start with: systemctl --user start hydeclaw-memory-worker".into()),
                ),
                Err(e) => CheckResult::warn(
                    format!("memory worker check failed: {}", e),
                    ms,
                    None,
                ),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("memory_worker"));

    #[cfg(not(target_os = "linux"))]
    let memory_worker_check = CheckResult {
        status: CheckStatus::Ok,
        message: "memory worker check not available on this platform".into(),
        latency_ms: None,
        fix_hint: None,
        details: None,
    };

    // ── 12. Provider reachability check ──────────────────────────────────
    let (providers_check, security_check) = tokio::join!(
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            check_provider_reachability(&infra, &auth),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(12),
            check_security_audit(&infra),
        ),
    );
    let providers_check = providers_check.unwrap_or_else(|_| CheckResult::timeout("providers"));
    let security_check = security_check.unwrap_or_else(|_| CheckResult::timeout("security_audit"));

    // ── 13. Disk space check (Linux only) ─────────────────────────────────
    #[cfg(target_os = "linux")]
    let disk_check = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            let start = std::time::Instant::now();
            let out = tokio::process::Command::new("df")
                .args(["-k", "--output=avail", "."])
                .output()
                .await;
            let ms = start.elapsed().as_millis() as u64;
            match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let avail_kb: i64 = text
                        .lines()
                        .nth(1)
                        .and_then(|l| l.trim().parse().ok())
                        .unwrap_or(i64::MAX);
                    let avail_mb = avail_kb / 1024;
                    if avail_kb < 102_400 {
                        CheckResult::error(
                            format!("{} MB disk free (critical)", avail_mb),
                            ms,
                            Some("free disk space immediately — system may become unstable".into()),
                        )
                    } else if avail_kb < 512_000 {
                        CheckResult::warn(
                            format!("{} MB disk free (low)", avail_mb),
                            ms,
                            Some("consider freeing disk space or expanding storage".into()),
                        )
                    } else {
                        CheckResult::ok(format!("{} MB disk free", avail_mb), ms)
                    }
                }
                Ok(o) => CheckResult::warn(
                    format!(
                        "df exited with error: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                    ms,
                    None,
                ),
                Err(e) => CheckResult::warn(format!("disk check failed: {}", e), ms, None),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("disk"));

    #[cfg(not(target_os = "linux"))]
    let disk_check = CheckResult {
        status: CheckStatus::Ok,
        message: "disk check not available on this platform".into(),
        latency_ms: None,
        fix_hint: None,
        details: None,
    };

    // ── 15. Network discovery check ──────────────────────────────────────────
    let network_check = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            let start = std::time::Instant::now();
            let summary = super::network::fetch_network_summary(&status).await;
            let ms = start.elapsed().as_millis() as u64;
            let mut cr = CheckResult::ok("network discovery available", ms);
            cr.details = Some(summary);
            cr
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("network"));

    // ── Backup status check ─────────────────────────────────────────────────
    let backup_check = {
        let config = cfg_svc.shared_config.read().await;
        let backup_cfg = &config.backup;
        let enabled = backup_cfg.enabled;
        let cron = backup_cfg.cron.clone();
        let retention_days = backup_cfg.retention_days;
        drop(config);

        if enabled {
            // Find most recent backup file (current format: hydeclaw-YYYY-MM-DD.tar.gz).
            let mut latest: Option<(String, u64, chrono::DateTime<chrono::Utc>)> = None;
            if let Ok(mut dir) = tokio::fs::read_dir("backups").await {
                while let Ok(Some(entry)) = dir.next_entry().await {
                    let path = entry.path();
                    let is_backup = path.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".tar.gz"));
                    if is_backup
                        && let Ok(meta) = entry.metadata().await {
                            let modified = meta.modified().ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .and_then(|d| chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0));
                            if let Some(ts) = modified
                                && latest.as_ref().is_none_or(|(_, _, prev)| ts > *prev) {
                                    latest = Some((
                                        path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                                        meta.len(),
                                        ts,
                                    ));
                                }
                        }
                }
            }

            if let Some((filename, size_bytes, created_at)) = latest {
                let age_hours = (chrono::Utc::now() - created_at).num_hours();
                let status = if age_hours > 48 {
                    CheckStatus::Warn
                } else {
                    CheckStatus::Ok
                };
                let message = format!("last backup: {} ({} ago)", filename,
                    if age_hours < 1 { "< 1h".to_string() }
                    else if age_hours < 24 { format!("{age_hours}h") }
                    else { format!("{}d", age_hours / 24) }
                );
                let mut cr = CheckResult {
                    status,
                    message,
                    latency_ms: None,
                    fix_hint: if age_hours > 48 { Some("backup is stale — check cron schedule or run POST /api/backup".into()) } else { None },
                    details: None,
                };
                cr.details = Some(json!({
                    "enabled": true,
                    "cron": cron,
                    "retention_days": retention_days,
                    "last_backup": filename,
                    "last_backup_at": created_at,
                    "size_bytes": size_bytes,
                }));
                cr
            } else {
                let mut cr = CheckResult::warn(
                    "no backups found",
                    0,
                    Some("run POST /api/backup to create first backup".into()),
                );
                cr.details = Some(json!({
                    "enabled": true,
                    "cron": cron,
                    "retention_days": retention_days,
                }));
                cr
            }
        } else {
            let mut cr = CheckResult::warn(
                "automatic backups disabled",
                0,
                Some("enable in hydeclaw.toml: [backup] enabled = true".into()),
            );
            cr.details = Some(json!({
                "enabled": false,
                "cron": cron,
                "retention_days": retention_days,
            }));
            cr
        }
    };

    // ── Compute overall status ─────────────────────────────────────────────
    let all_checks = [
        &database_check,
        &toolgate_check,
        &migrations_check,
        &pgvector_check,
        &memory_worker_check,
        &disk_check,
        &browser_renderer_check,
        &searxng_check,
        &secrets_check,
        &channels_check,
        &agents_check,
        &tool_health_check,
        &providers_check,
        &security_check,
        &network_check,
        &backup_check,
    ];
    let all_ok = all_checks.iter().all(|c| !matches!(c.status, CheckStatus::Error));

    Json(json!({
        "ok": all_ok,
        "checks": {
            "database": database_check,
            "toolgate": toolgate_check,
            "migrations": migrations_check,
            "pgvector": pgvector_check,
            "memory_worker": memory_worker_check,
            "disk": disk_check,
            "browser_renderer": browser_renderer_check,
            "searxng": searxng_check,
            "secrets": secrets_check,
            "channels": channels_check,
            "agents": agents_check,
            "tool_health": tool_health_check,
            "providers": providers_check,
            "security_audit": security_check,
            "network": network_check,
            "backup": backup_check,
        }
    }))
}

// ── Watchdog API ──

/// GET /api/watchdog/status
pub(crate) async fn api_watchdog_status() -> impl IntoResponse {
    match tokio::fs::read_to_string("/tmp/hydeclaw-watchdog.json").await {
        Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(v) => Json(v).into_response(),
            Err(_) => Json(json!({"error": "invalid status file"})).into_response(),
        },
        Err(_) => Json(json!({"error": "watchdog not running"})).into_response(),
    }
}

/// GET /api/watchdog/config
pub(crate) async fn api_watchdog_config() -> impl IntoResponse {
    match tokio::fs::read_to_string("config/watchdog.toml").await {
        Ok(text) => Json(json!({"config": text})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// POST /api/watchdog/restart/{name} — execute `restart_cmd` for a watchdog check
pub(crate) async fn api_watchdog_restart_check(
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let config_text = match tokio::fs::read_to_string("config/watchdog.toml").await {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let config: toml::Value = match toml::from_str(&config_text) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let checks = config.get("checks").and_then(|v| v.as_array());
    let restart_cmd = checks.and_then(|arr| {
        arr.iter().find(|c| c.get("name").and_then(|n| n.as_str()) == Some(&name))
            .and_then(|c| c.get("restart_cmd").and_then(|r| r.as_str()))
    });
    let Some(cmd) = restart_cmd else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": format!("no restart_cmd for check '{}'", name)}))).into_response();
    };
    tracing::info!(check = %name, cmd, "watchdog restart requested via API");
    let output = tokio::process::Command::new("bash").args(["-c", cmd]).output().await;
    match output {
        Ok(o) if o.status.success() => Json(json!({"ok": true, "check": name})).into_response(),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": err.to_string()}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}

/// GET /api/watchdog/settings — read alerting settings from DB
pub(crate) async fn api_watchdog_settings(
    State(infra): State<InfraServices>,
) -> Json<Value> {
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT key, value FROM watchdog_settings",
    )
    .fetch_all(&infra.db)
    .await
    .unwrap_or_default();

    let mut settings = serde_json::Map::new();
    for (key, value) in rows {
        settings.insert(key, value);
    }
    Json(Value::Object(settings))
}

/// PUT /api/watchdog/settings — update alerting settings
pub(crate) async fn api_watchdog_settings_update(
    State(infra): State<InfraServices>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(obj) = body.as_object() else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "expected JSON object"}))).into_response();
    };

    let allowed = ["alert_channel_ids", "alert_events"];
    for (key, value) in obj {
        if !allowed.contains(&key.as_str()) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("unknown key: {}", key)}))).into_response();
        }
        if let Err(e) = sqlx::query(
            "INSERT INTO watchdog_settings (key, value, updated_at) VALUES ($1, $2, now())
             ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = now()",
        )
        .bind(key)
        .bind(value)
        .execute(&infra.db)
        .await
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    }

    Json(json!({"ok": true})).into_response()
}

/// PUT /api/watchdog/config
pub(crate) async fn api_watchdog_config_update(Json(req): Json<serde_json::Value>) -> impl IntoResponse {
    let text = match req.get("config").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "config field required"}))).into_response(),
    };
    if toml::from_str::<toml::Value>(text).is_err() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid TOML"}))).into_response();
    }
    match tokio::fs::write("config/watchdog.toml", text).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}
