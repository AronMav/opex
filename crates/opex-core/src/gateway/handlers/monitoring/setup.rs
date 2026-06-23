//! Setup-wizard endpoints: `/api/setup/{status,complete,requirements}`
//! plus the `setup_guard_middleware` that 403s on `setup_complete`.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::{Value, json};

use super::{CheckResult, CheckStatus};
use crate::agent::cli_backend::CLI_PRESETS;
use crate::gateway::clusters::InfraServices;

pub(crate) async fn api_setup_status(State(infra): State<InfraServices>) -> Json<Value> {
    let complete: bool = opex_db::sys_flags::get(&infra.db, "setup_complete")
        .await
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Json(json!({ "needs_setup": !complete }))
}

/// POST /api/setup/complete — mark setup as done; guarded by `setup_guard_middleware`
pub(crate) async fn api_setup_complete(State(infra): State<InfraServices>) -> impl IntoResponse {
    let result = opex_db::sys_flags::upsert(&infra.db, "setup_complete", json!(true)).await;

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
    let complete: bool = opex_db::sys_flags::get(&infra.db, "setup_complete")
        .await
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
