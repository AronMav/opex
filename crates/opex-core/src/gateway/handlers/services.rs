use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{post},
};
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::{ConfigServices, InfraServices};
use crate::process_manager::ProcessManager;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/services/{name}/{action}", post(api_service_action))
        .route("/api/containers/{name}/restart", post(api_container_restart))
}

/// Handle restart/start/stop/status/logs for a native managed process.
///
/// `restart`/`rebuild` are fire-and-forget: they spawn into the process-wide
/// task tracker and return 202 immediately. Starting/stopping native services
/// can take seconds (SIGTERM grace period + re-spawn) and must not block the
/// HTTP client. Other actions remain synchronous because they are fast or
/// inherently query-only.
async fn handle_managed_action(
    pm: &Arc<ProcessManager>,
    infra: &crate::gateway::clusters::InfraServices,
    name: &str,
    action: &str,
) -> (StatusCode, Json<Value>) {
    match action {
        "restart" | "rebuild" => {
            let pm = pm.clone();
            let bg_name = name.to_string();
            let bg_action = action.to_string();
            let restart_lock = infra.restart_lock(name).await;
            infra.spawn_bg(async move {
                // Hold the per-name lock for the duration of the restart so
                // two concurrent POST /api/services/{name}/restart calls
                // serialize instead of racing the process manager.
                let _lock = restart_lock;
                if let Err(e) = pm.restart(&bg_name).await {
                    tracing::error!(
                        service = %bg_name,
                        action = %bg_action,
                        error = %e,
                        "background service restart failed"
                    );
                } else {
                    tracing::info!(
                        service = %bg_name,
                        action = %bg_action,
                        "background service restart completed"
                    );
                }
            });
            (
                StatusCode::ACCEPTED,
                Json(json!({"ok": true, "action": action, "service": name, "managed": true, "queued": true})),
            )
        }
        "start" => match pm.start(name).await {
            Ok(()) => (StatusCode::OK, Json(json!({"ok": true, "action": "start", "service": name, "managed": true}))),
            Err(e) => (StatusCode::CONFLICT, Json(json!({"ok": false, "error": e.to_string()}))),
        },
        "stop" => match pm.kill(name).await {
            Ok(()) => (StatusCode::OK, Json(json!({"ok": true, "action": "stop", "service": name, "managed": true}))),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))),
        },
        "status" => {
            let s = pm.status(name).await;
            (StatusCode::OK, Json(json!({"ok": true, "running": s.running, "restart_count": s.restart_count, "managed": true})))
        }
        "logs" => (
            StatusCode::OK,
            Json(json!({"ok": true, "logs": "Native process logs go to stdout. Use: journalctl -u opex-core -f"})),
        ),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": format!("action '{}' is not supported for native managed processes (supported: restart, start, stop, status, logs)", action)})),
        ),
    }
}

/// Run `docker compose ps --format json <service>` and parse the JSON result.
async fn docker_compose_ps(compose_file: &str, service: &str) -> Option<Value> {
    let args = ["compose", "-f", compose_file, "ps", "--format", "json", service];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("docker").args(args).output(),
    ).await;
    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            serde_json::from_str(stdout.trim()).ok()
        }
        _ => None,
    }
}

pub(crate) async fn api_service_action(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
    Path((name, action)): Path<(String, String)>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    if !matches!(action.as_str(), "rebuild" | "restart" | "stop" | "start" | "logs" | "status" | "exec" | "inspect") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "action must be 'rebuild', 'restart', 'stop', 'start', 'logs', 'status', 'inspect', or 'exec'"})),
        )
            .into_response();
    }

    // Managed native processes take priority over Docker
    if let Some(ref pm) = infra.process_manager
        && pm.is_managed(&name) {
            let (status, body) = handle_managed_action(pm, &infra, &name, &action).await;
            return (status, body).into_response();
        }

    let config = cfg.shared_config.read().await;
    if !config.docker.rebuild_allowed.iter().any(|s| s == &name) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "error": format!("service '{}' is not managed natively and not in rebuild_allowed whitelist", name)})),
        )
            .into_response();
    }
    let compose_file = config.docker.compose_file.clone();
    let timeout = config.docker.rebuild_timeout_secs;
    drop(config);

    tracing::info!(service = %name, action = %action, "docker service action requested");

    // Exec action: run command inside container (toolgate only)
    if action == "exec" {
        if name != "toolgate" && name != "channels" {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"ok": false, "error": "exec is only allowed for the 'toolgate' service"})),
            )
                .into_response();
        }
        let command = body
            .as_ref()
            .and_then(|b| b.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if command.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "exec requires 'command' in request body"})),
            )
                .into_response();
        }
        // Allowlist: only known-safe diagnostic commands are permitted.
        // python/perl/ruby are NOT allowed — blocklist approach is fundamentally bypassable.
        let safe_binaries: &[&str] = &[
            "ls", "cat", "head", "tail", "grep", "find", "wc", "file", "stat",
            "df", "du", "ps", "id", "whoami", "hostname", "uname", "date",
            "tree", "pwd", "sort", "uniq", "diff", "md5sum", "sha256sum",
            "pip", "pip3", "env", "printenv",
        ];
        // F125: reject control characters FIRST. The command is executed via
        // `sh -c`, and the per-segment metachar guard below has no newline in its
        // list while split_whitespace/split('|') treat '\n' as whitespace — so
        // `id\ncat /etc/shadow` passed with first token `id` and ran BOTH lines.
        // A newline/CR/tab has no legitimate place in an allow-listed read-only
        // command; blocking them closes the multi-command injection.
        if command.contains(['\n', '\r', '\t']) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"ok": false, "error": "control characters (newline/CR/tab) are not allowed"})),
            ).into_response();
        }
        // Block shell substitution/expansion metacharacters
        if command.contains("$(") || command.contains('`') || command.contains("${") {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"ok": false, "error": "shell substitution ($(), ``, ${}) is not allowed"})),
            ).into_response();
        }
        // Validate each pipe segment: "pip list | head -5" -> ["pip list", "head -5"]
        let segments: Vec<&str> = command.split('|').map(str::trim).collect();
        let mut all_safe = true;
        let mut blocked_cmd = "";
        for seg in &segments {
            let first = seg.split_whitespace().next().unwrap_or("");
            if !safe_binaries.contains(&first) {
                all_safe = false;
                blocked_cmd = first;
                break;
            }
            // Block shell metacharacters within segments
            let dangerous_patterns = [";", "&&", "||", "$(", "`", ">", ">>", "<", "<<"];
            for pattern in &dangerous_patterns {
                if seg.contains(pattern) {
                    tracing::warn!(command = %command, segment = %seg, pattern = %pattern, "docker exec blocked: shell metacharacter");
                    return (
                        StatusCode::FORBIDDEN,
                        Json(json!({"ok": false, "error": format!("shell metacharacter '{}' not allowed", pattern)})),
                    ).into_response();
                }
            }
        }
        if !all_safe {
            tracing::warn!(command = %command, "docker exec blocked: not in allowlist");
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"ok": false, "error": format!("command '{}' not allowed. Permitted: ls, cat, head, tail, grep, find, pip list/show/freeze, python -c (safe imports)", blocked_cmd)})),
            )
                .into_response();
        }
        // Safety: command is validated against an allowlist and filtered for shell metacharacters above;
        // tokio::process::Command passes arguments as an array (no shell interpolation).
        let args = vec![
            "compose".to_string(), "-f".to_string(), compose_file.clone(),
            "exec".to_string(), "-T".to_string(), name.clone(),
            "sh".to_string(), "-c".to_string(), command,
        ];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new("docker").args(&args).output(),
        )
        .await;
        return match result {
            Ok(Ok(output)) => {
                let stdout: String = String::from_utf8_lossy(&output.stdout).chars().take(8000).collect();
                let stderr: String = String::from_utf8_lossy(&output.stderr).chars().take(4000).collect();
                // The allowlist permits `env`/`printenv`; redact before this
                // reaches the API response (T02 triage Пункт 5).
                let stdout = crate::redact::redact_terminal_output(&stdout);
                let stderr = crate::redact::redact_terminal_output(&stderr);
                Json(json!({"ok": output.status.success(), "exit_code": output.status.code(), "stdout": stdout, "stderr": stderr})).into_response()
            }
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": format!("failed to spawn docker: {}", e)}))).into_response(),
            Err(_) => (StatusCode::GATEWAY_TIMEOUT, Json(json!({"ok": false, "error": "exec command timed out (30s)"}))).into_response(),
        };
    }

    // Inspect action: container details (ports, mounts, env with secrets masked)
    if action == "inspect" {
        let cid = match docker_compose_ps(&compose_file, &name).await
            .and_then(|v| v.get("ID").or(v.get("id")).and_then(|id| id.as_str().map(std::string::ToString::to_string)))
        {
            Some(id) => id,
            None => return Json(json!({"ok": false, "error": "container not found or not running"})).into_response(),
        };
        // docker inspect with Go template for safe fields only
        let fmt = "{{json .Config.ExposedPorts}}|||{{json .NetworkSettings.Ports}}|||{{json .Mounts}}|||{{json .Config.Env}}|||{{.State.Status}}|||{{.State.Health.Status}}|||{{.Created}}|||{{json .Config.Image}}";
        let inspect_args = ["inspect", "--format", fmt, &cid];
        let inspect_result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new("docker").args(inspect_args).output(),
        )
        .await;
        return match inspect_result {
            Ok(Ok(output)) => {
                let raw = String::from_utf8_lossy(&output.stdout);
                let parts: Vec<&str> = raw.trim().splitn(8, "|||").collect();
                // Mask environment variables that look like secrets
                let env_raw = parts.get(3).unwrap_or(&"[]");
                let env_masked: Vec<String> = serde_json::from_str::<Vec<String>>(env_raw)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| {
                        if let Some((k, v)) = e.split_once('=') {
                            // F129: deny-by-default. The old keyword+>64 heuristic
                            // leaked short secrets with non-keyword names
                            // (CREDENTIALS, PASSPHRASE, PWD, AUTH, COOKIE, …). Mask
                            // every value except an explicit allowlist of known-benign
                            // infra keys.
                            const BENIGN: &[&str] = &[
                                "PATH", "HOME", "HOSTNAME", "LANG", "LC_ALL", "TZ",
                                "TERM", "PYTHONPATH", "PYTHONUNBUFFERED", "PYTHON_VERSION",
                                "NODE_VERSION", "LD_LIBRARY_PATH", "DEBIAN_FRONTEND",
                            ];
                            if BENIGN.contains(&k.to_ascii_uppercase().as_str()) {
                                e
                            } else {
                                format!("{}={}", k, super::secrets::mask_secret_value(v))
                            }
                        } else {
                            e
                        }
                    })
                    .collect();
                Json(json!({
                    "ok": true,
                    "exposed_ports": serde_json::from_str::<Value>(parts.first().unwrap_or(&"{}")).unwrap_or(json!({})),
                    "port_bindings": serde_json::from_str::<Value>(parts.get(1).unwrap_or(&"{}")).unwrap_or(json!({})),
                    "mounts": serde_json::from_str::<Value>(parts.get(2).unwrap_or(&"[]")).unwrap_or(json!([])),
                    "env": env_masked,
                    "state": parts.get(4).unwrap_or(&"unknown"),
                    "health": parts.get(5).unwrap_or(&""),
                    "created": parts.get(6).unwrap_or(&""),
                    "image": serde_json::from_str::<Value>(parts.get(7).unwrap_or(&"\"\"")).unwrap_or(json!("")),
                })).into_response()
            }
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": format!("inspect failed: {}", e)}))).into_response(),
            Err(_) => (StatusCode::GATEWAY_TIMEOUT, Json(json!({"ok": false, "error": "inspect timed out"}))).into_response(),
        };
    }

    // Logs action: return container logs with optional filters (since, grep, tail)
    if action == "logs" {
        let tail = body.as_ref()
            .and_then(|b| b.get("tail")).and_then(serde_json::Value::as_u64)
            .unwrap_or(100).min(500).to_string();
        let since = body.as_ref()
            .and_then(|b| b.get("since")).and_then(|v| v.as_str()).map(std::string::ToString::to_string);
        let grep = body.as_ref()
            .and_then(|b| b.get("grep")).and_then(|v| v.as_str()).map(std::string::ToString::to_string);

        let mut args = vec![
            "compose".to_string(), "-f".to_string(), compose_file.clone(),
            "logs".to_string(), "--tail".to_string(), tail, "--no-color".to_string(),
        ];
        if let Some(ref s) = since {
            args.push("--since".to_string());
            args.push(s.clone());
        }
        args.push(name.clone());

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::process::Command::new("docker").args(&args).output(),
        )
        .await;
        return match result {
            Ok(Ok(output)) => {
                let raw = String::from_utf8_lossy(&output.stdout);
                // Apply grep filter before truncation (docker compose logs doesn't support --grep)
                let logs: String = if let Some(ref pattern) = grep {
                    let pattern_lower = pattern.to_lowercase();
                    raw.lines()
                        .filter(|line| line.to_lowercase().contains(&pattern_lower))
                        .collect::<Vec<_>>()
                        .join("\n")
                        .chars().take(8000).collect()
                } else {
                    raw.chars().take(8000).collect()
                };
                let stderr: String = String::from_utf8_lossy(&output.stderr).chars().take(2000).collect();
                Json(json!({"ok": output.status.success(), "logs": logs, "stderr": stderr})).into_response()
            }
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": format!("failed to spawn docker: {}", e)}))).into_response(),
            Err(_) => (StatusCode::GATEWAY_TIMEOUT, Json(json!({"ok": false, "error": "timeout reading logs"}))).into_response(),
        };
    }

    // Status action: show container state (running/stopped/health)
    if action == "status" {
        return match docker_compose_ps(&compose_file, &name).await {
            Some(info) => Json(json!({"ok": true, "container": info})).into_response(),
            None => Json(json!({"ok": false, "error": "container not found or docker timed out"})).into_response(),
        };
    }

    let args: Vec<String> = match action.as_str() {
        "rebuild" => ["compose", "-f", &compose_file, "up", "-d", "--build", "--no-deps", &name]
            .iter().map(|s| (*s).to_string()).collect(),
        "restart" => ["compose", "-f", &compose_file, "restart", &name]
            .iter().map(|s| (*s).to_string()).collect(),
        "stop" => ["compose", "-f", &compose_file, "stop", &name]
            .iter().map(|s| (*s).to_string()).collect(),
        "start" => ["compose", "-f", &compose_file, "start", &name]
            .iter().map(|s| (*s).to_string()).collect(),
        _ => unreachable!(),
    };

    // Restart/rebuild/start can take many seconds (container stop + start +
    // health window). Return 202 immediately and run the action in the
    // background task tracker. Stop remains synchronous because it is fast.
    if matches!(action.as_str(), "restart" | "rebuild" | "start") {
        let bg_name = name.clone();
        let bg_action = action.clone();
        let compose_file = compose_file.clone();
        // Acquire the per-name serialization lock before spawning so two
        // concurrent requests targeting the same service queue instead of
        // racing docker-compose (which can deadlock on stop+start of the
        // same container).
        let restart_lock = infra.restart_lock(&name).await;
        infra.spawn_bg(async move {
            let _lock = restart_lock;
            run_docker_action(
                &compose_file,
                &bg_name,
                &bg_action,
                timeout,
            )
            .await;
        });
        return (
            StatusCode::ACCEPTED,
            Json(json!({"ok": true, "action": action, "service": name, "queued": true})),
        )
            .into_response();
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout),
        tokio::process::Command::new("docker").args(&args).output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let ok = output.status.success();
            let stdout: String = String::from_utf8_lossy(&output.stdout).chars().take(4000).collect();
            let stderr: String = String::from_utf8_lossy(&output.stderr).chars().take(4000).collect();
            if ok {
                tracing::info!(service = %name, action = %action, "docker service action succeeded");
            } else {
                tracing::warn!(service = %name, action = %action, stderr = %stderr, "docker service action failed");
            }
            (
                if ok { StatusCode::OK } else { StatusCode::INTERNAL_SERVER_ERROR },
                Json(json!({
                    "ok": ok,
                    "exit_code": output.status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                })),
            )
                .into_response()
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "failed to spawn docker command");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": format!("failed to spawn docker: {}", e)})),
            )
                .into_response()
        }
        Err(_) => {
            tracing::error!(service = %name, timeout_secs = timeout, "docker command timed out");
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"ok": false, "error": format!("timeout after {}s", timeout)})),
            )
                .into_response()
        }
    }
}

/// Run a docker-compose action and log the outcome. Used as a background task
/// for restart/rebuild/start so the HTTP caller is not blocked.
async fn run_docker_action(
    compose_file: &str,
    name: &str,
    action: &str,
    timeout: u64,
) {
    let args: Vec<String> = match action {
        "rebuild" => ["compose", "-f", compose_file, "up", "-d", "--build", "--no-deps", name]
            .iter().map(|s| (*s).to_string()).collect(),
        "restart" => ["compose", "-f", compose_file, "restart", name]
            .iter().map(|s| (*s).to_string()).collect(),
        // `start` without --no-deps would also start linked dependencies, which
        // can cascade into restarting unrelated services. We only want to start
        // the explicitly requested container; `restart` is already scoped by
        // docker-compose so it doesn't need the flag.
        "start" => ["compose", "-f", compose_file, "start", "--no-deps", name]
            .iter().map(|s| (*s).to_string()).collect(),
        _ => return,
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout),
        tokio::process::Command::new("docker").args(&args).output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let ok = output.status.success();
            let stdout: String = String::from_utf8_lossy(&output.stdout).chars().take(4000).collect();
            let stderr: String = String::from_utf8_lossy(&output.stderr).chars().take(4000).collect();
            if ok {
                tracing::info!(service = %name, action = %action, stdout = %stdout, "background docker service action succeeded");
                // Best-effort post-action health check
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if let Some(v) = docker_compose_ps(compose_file, name).await {
                    let state = v.get("State").or(v.get("state")).and_then(|s| s.as_str()).unwrap_or("unknown");
                    let health = v.get("Health").or(v.get("health")).and_then(|s| s.as_str()).unwrap_or("");
                    let status = v.get("Status").or(v.get("status")).and_then(|s| s.as_str()).unwrap_or("");
                    tracing::info!(service = %name, state, health, status, "background docker health check");
                }
            } else {
                tracing::warn!(service = %name, action = %action, stderr = %stderr, "background docker service action failed");
            }
        }
        Ok(Err(e)) => {
            tracing::error!(service = %name, action = %action, error = %e, "background docker command spawn failed");
        }
        Err(_) => {
            tracing::error!(service = %name, action = %action, timeout_secs = timeout, "background docker command timed out");
        }
    }
}

/// POST /api/containers/{name}/restart — restart any Docker container by name.
/// Returns 202 immediately and performs the restart in the background task
/// tracker. Container restart can take seconds and should not block the HTTP
/// caller.
pub(crate) async fn api_container_restart(
    State(infra): State<InfraServices>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // Whitelist of containers that may be restarted (from docker-compose.yml).
    // Database and security-sensitive containers are intentionally excluded.
    const RESTART_ALLOWED: &[&str] = &[
        "browser-renderer",
        "mcp-stock-analysis",
        "mcp-weather",
        "mcp-obsidian",
        "mcp-browser-cdp",
        "mcp-postgres",
        "mcp-fetch",
        "mcp-memory",
        "mcp-sequential-thinking",
        "mcp-time",
        "mcp-filesystem",
        "mcp-git",
        "mcp-notion",
        "mcp-todoist",
    ];
    if !RESTART_ALLOWED.iter().any(|allowed| name == *allowed) {
        tracing::warn!(container = %name, "container restart blocked: not in whitelist");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": format!("container '{}' restart not allowed", name)})),
        )
            .into_response();
    }

    // Synchronously verify the container exists before queueing the restart.
    // Without this check a typo or a stale whitelist entry (e.g. an MCP
    // removed from docker-compose.yml) returns 202 and the operator only
    // discovers the "No such container" error in the logs after waiting for
    // the 202 to "complete". docker inspect is cheap (~10ms local daemon).
    let inspect = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new("docker").args(["inspect", &name]).output(),
    )
    .await;
    let container_exists = match inspect {
        Ok(Ok(o)) => o.status.success(),
        _ => false,
    };
    if !container_exists {
        tracing::warn!(container = %name, "container restart blocked: no such container");
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("container '{}' not found", name)})),
        )
            .into_response();
    }

    tracing::info!(container = %name, "container restart requested; queuing background restart");
    let name_clone = name.clone();
    // Per-container serialization: two concurrent POST /api/containers/{name}/restart
    // would race docker on the same stop+start cycle. Acquire the lock before
    // spawning so the second request blocks at the entry of its spawn_bg
    // closure rather than inside docker.
    let restart_lock = infra.restart_lock(&name).await;
    infra.spawn_bg(async move {
        let _lock = restart_lock;
        match tokio::process::Command::new("docker")
            .args(["restart", &name_clone])
            .output()
            .await
        {
            Ok(o) if o.status.success() => {
                tracing::info!(container = %name_clone, "background container restart succeeded");
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                tracing::warn!(container = %name_clone, stderr = %err, "background container restart failed");
            }
            Err(e) => {
                tracing::error!(container = %name_clone, error = %e, "background container restart spawn failed");
            }
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "container": name, "queued": true})),
    )
        .into_response()
}
