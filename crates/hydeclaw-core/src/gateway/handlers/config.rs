use axum::{
    Router,
    body::Body,
    extract::{FromRequest, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, ConfigServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/config/schema", get(api_get_config_schema))
        .route("/api/config", get(api_get_config).put(api_update_config))
        .route("/api/config/export", get(api_export_config))
        .route("/api/config/import", post(api_import_config))
        .route("/api/restart", post(api_restart))
        .route("/api/tts/voices", get(api_tts_voices))
        .route("/api/tts/synthesize", post(api_tts_synthesize))
        .route("/api/canvas/{agent}", get(api_canvas_state).delete(api_canvas_clear))
}

/// Shared reqwest client for Toolgate HTTP calls (voices + synthesize).
static TOOLGATE_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

/// Return the current canvas state for a given agent (or null if empty).
pub(crate) async fn api_canvas_state(
    State(agents): State<AgentCore>,
    axum::extract::Path(agent): axum::extract::Path<String>,
) -> impl IntoResponse {
    let engine = match agents.get_engine(&agent).await {
        Some(e) => e,
        None => return Json(json!({"visible": false})).into_response(),
    };
    let guard = engine.tex().canvas_state.read().await;
    match guard.as_ref() {
        Some(cs) => {
            let action = if cs.content_type == "json" { "push_data" } else { "present" };
            Json(json!({
                "visible": true,
                "agent": agent,
                "action": action,
                "content_type": cs.content_type,
                "content": cs.content,
                "title": cs.title,
            })).into_response()
        }
        None => Json(json!({"visible": false})).into_response(),
    }
}

pub(crate) async fn api_canvas_clear(
    State(agents): State<AgentCore>,
    axum::extract::Path(agent): axum::extract::Path<String>,
) -> StatusCode {
    if let Some(engine) = agents.get_engine(&agent).await {
        let mut guard = engine.tex().canvas_state.write().await;
        *guard = None;
    }
    StatusCode::NO_CONTENT
}

pub(crate) async fn api_tts_voices(State(agents): State<AgentCore>) -> impl IntoResponse {
    let toolgate_url = {
        let deps = agents.deps.read().await;
        deps.toolgate_url.clone()
    };
    let Some(base) = toolgate_url else {
        return Json(json!({"voices": []})).into_response();
    };
    let url = format!("{}/audio/voices", base.trim_end_matches('/'));
    let client = TOOLGATE_CLIENT.get_or_init(reqwest::Client::new);
    match client.get(&url).timeout(std::time::Duration::from_secs(5)).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<serde_json::Value>().await {
                Ok(data) => Json(data).into_response(),
                Err(_) => Json(json!({"voices": []})).into_response(),
            }
        }
        _ => Json(json!({"voices": []})).into_response(),
    }
}

/// POST /api/tts/synthesize — synthesize speech via Toolgate
pub(crate) async fn api_tts_synthesize(
    State(agents): State<AgentCore>,
    Json(body): Json<TtsSynthesizeRequest>,
) -> impl IntoResponse {
    let toolgate_url = {
        let deps = agents.deps.read().await;
        deps.toolgate_url.clone()
    };
    let Some(base) = toolgate_url else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Toolgate URL not configured").into_response();
    };

    let client = TOOLGATE_CLIENT.get_or_init(reqwest::Client::new);
    let resp = client
        .post(format!("{}/v1/audio/speech", base.trim_end_matches('/')))
        .json(&serde_json::json!({
            "input": body.text,
            "voice": body.voice.unwrap_or_default(),
            "model": body.model.unwrap_or_else(|| "tts-1".to_string()),
        }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let content_type = r.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("audio/mpeg")
                .to_string();
            let bytes = r.bytes().await.unwrap_or_default();
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, content_type)],
                bytes,
            ).into_response()
        }
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            (StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY), text).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("Toolgate error: {e}")).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct TtsSynthesizeRequest {
    text: String,
    voice: Option<String>,
    model: Option<String>,
}

// ── Config API ──

/// GET /api/config/schema — return the JSON Schema for `AppConfig`.
///
/// Schema is generated once at first call and cached for the process lifetime.
/// The schema is static — it only changes when the binary is rebuilt.
pub(crate) async fn api_get_config_schema() -> impl IntoResponse {
    static CONFIG_SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    let schema = CONFIG_SCHEMA.get_or_init(|| {
        let root = schemars::schema_for!(crate::config::AppConfig);
        serde_json::to_value(root).unwrap_or(serde_json::Value::Null)
    });
    Json(schema.clone())
}

pub(crate) async fn api_get_config(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(cfg_svc): State<ConfigServices>,
) -> Json<Value> {
    let config = cfg_svc.shared_config.read().await;
    let embed_dim = infra.embedder.embed_dim();
    let embed_dim_val: Option<u32> = if embed_dim > 0 { Some(embed_dim) } else { None };

    // Return config structure without sensitive values
    Json(json!({
        "gateway": {
            "listen": config.gateway.listen,
            "auth_token_env": config.gateway.auth_token_env,
            "public_url": config.gateway.public_url,
        },
        "database": {
            "url": "***hidden***",
        },
        "limits": {
            "max_requests_per_minute": config.limits.max_requests_per_minute,
            "max_tool_concurrency": config.limits.max_tool_concurrency,
            "request_timeout_secs": config.limits.request_timeout_secs,
            "max_agent_turns": config.limits.max_agent_turns,
        },
        "subagents": {
            "enabled": config.subagents.enabled,
            "default_mode": config.subagents.default_mode,
            "max_concurrent_in_process": config.subagents.max_concurrent_in_process,
            "max_concurrent_docker": config.subagents.max_concurrent_docker,
            "docker_timeout": config.subagents.docker_timeout,
            "in_process_timeout": config.subagents.in_process_timeout,
        },
        "docker": {
            "compose_file": config.docker.compose_file,
            "rebuild_allowed": config.docker.rebuild_allowed,
            "rebuild_timeout_secs": config.docker.rebuild_timeout_secs,
        },
        "tools_count": agents.tools.len().await,
        "mcp_count": config.mcp.len(),
        "tools": agents.tools.entries().await,
        "mcp": config.mcp.keys().collect::<Vec<_>>(),
        "memory": {
            "enabled": config.memory.enabled,
            "embed_dim": embed_dim_val,
            "embed_dimensions": config.memory.embed_dimensions,
            "available": infra.embedder.is_available(),
        },
        "toolgate_url": agents.deps.read().await.toolgate_url,
        "sandbox": {
            "enabled": config.sandbox.enabled,
            "image": config.sandbox.image,
            "timeout_secs": config.sandbox.timeout_secs,
            "memory_mb": config.sandbox.memory_mb,
            "extra_binds": config.sandbox.extra_binds,
        },
        "backup": {
            "enabled": config.backup.enabled,
            "cron": config.backup.cron,
            "retention_days": config.backup.retention_days,
        },
        "agent_tool": {
            "message_wait_for_idle_secs": config.agent_tool.message_wait_for_idle_secs,
            "message_result_secs": config.agent_tool.message_result_secs,
            "safety_timeout_secs": config.agent_tool.safety_timeout_secs,
        },
    }))
}

// ── Config Update API ──

#[derive(serde::Deserialize)]
pub(crate) struct ConfigUpdatePayload {
    toolgate_url: Option<String>,
    embed_enabled: Option<bool>,
    embed_dim: Option<u32>,
    embed_dimensions: Option<u32>,
    subagents_enabled: Option<bool>,
    max_requests_per_minute: Option<u32>,
    max_tool_concurrency: Option<u32>,
    max_agent_turns: Option<usize>,
    public_url: Option<String>,
    backup_enabled: Option<bool>,
    backup_cron: Option<String>,
    backup_retention_days: Option<u32>,
    // [agent_tool] — multi-agent timeouts (UI-configurable).
    agent_tool_message_wait_for_idle_secs: Option<u64>,
    agent_tool_message_result_secs: Option<u64>,
    agent_tool_safety_timeout_secs: Option<u64>,
}

pub(crate) async fn api_update_config(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(cfg_svc): State<ConfigServices>,
    Json(payload): Json<ConfigUpdatePayload>,
) -> impl IntoResponse {
    // Structured validation — build proposed config and validate before writing
    {
        let current = cfg_svc.shared_config.read().await.clone();
        let mut proposed = current.clone();
        if let Some(ref url) = payload.toolgate_url {
            proposed.toolgate_url = if url.is_empty() { None } else { Some(url.clone()) };
        }
        if let Some(ref url) = payload.public_url {
            proposed.gateway.public_url = if url.is_empty() { None } else { Some(url.clone()) };
        }
        let errors = crate::config::validate_config(&proposed);
        if !errors.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "errors": errors })),
            ).into_response();
        }
    }

    let config_path = "config/hydeclaw.toml";

    // Serialize config writes to prevent concurrent partial updates
    let _config_guard = cfg_svc.config_write_lock.lock().await;

    // Create backup before modifying — fail if unreadable (don't risk empty restore)
    let config_backup = match tokio::fs::read_to_string(config_path).await {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": format!("cannot read config for backup: {}", e)
            }))).into_response();
        }
    };

    // Tell the file watcher to skip the next change (we'll update in-memory config ourselves).
    // Set AFTER backup read so the flag is never leaked if the read fails (no file write occurs).
    cfg_svc.config_api_write_flag.store(true, std::sync::atomic::Ordering::Release);

    // Helper: restore backup and return an error response.
    // Defined as a closure-like macro pattern since async closures can't capture by ref easily.
    macro_rules! restore_and_fail {
        ($label:expr, $err:expr) => {{
            if let Err(restore_err) = tokio::fs::write(config_path, &config_backup).await {
                tracing::error!(
                    error = %$err,
                    restore_error = %restore_err,
                    "config write failed AND backup restore failed"
                );
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                    "error": format!("{} AND restore failed: {}. Manual fix required.", $label, restore_err)
                }))).into_response();
            }
            tracing::warn!(error = %$err, "config write failed, restored backup");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": format!("{}: {}", $label, $err)
            }))).into_response();
        }};
    }

    // Update TOML config file
    if let Err(e) = crate::config::update_service_urls(
        "config/hydeclaw.toml",
        payload.toolgate_url.as_deref(),
    ) {
        restore_and_fail!("failed to update config file", e);
    }

    // Update memory config in TOML
    if (payload.embed_enabled.is_some() || payload.embed_dim.is_some() || payload.embed_dimensions.is_some())
        && let Err(e) = crate::config::update_memory_config(
            "config/hydeclaw.toml",
            payload.embed_enabled,
            None, // embed_url removed — managed by toolgate
            None, // embed_model removed — managed by toolgate
            payload.embed_dim,
            payload.embed_dimensions,
        ) {
            restore_and_fail!("failed to update memory config", e);
        }

    // Update subagents config in TOML
    if let Some(enabled) = payload.subagents_enabled
        && let Err(e) = crate::config::update_subagents_enabled("config/hydeclaw.toml", enabled) {
            restore_and_fail!("failed to update subagents config", e);
        }

    // Update limits config in TOML
    if (payload.max_requests_per_minute.is_some() || payload.max_tool_concurrency.is_some() || payload.max_agent_turns.is_some())
        && let Err(e) = crate::config::update_limits_config(
            "config/hydeclaw.toml",
            payload.max_requests_per_minute,
            payload.max_tool_concurrency,
            payload.max_agent_turns,
        )
    {
        restore_and_fail!("failed to update limits config", e);
    }

    // Update public_url in TOML
    if let Some(ref url) = payload.public_url
        && let Err(e) = crate::config::update_public_url("config/hydeclaw.toml", url)
    {
        restore_and_fail!("failed to update public_url config", e);
    }

    // Update backup config in TOML
    if (payload.backup_enabled.is_some() || payload.backup_cron.is_some() || payload.backup_retention_days.is_some())
        && let Err(e) = crate::config::update_backup_config(
            "config/hydeclaw.toml",
            payload.backup_enabled,
            payload.backup_cron.as_deref(),
            payload.backup_retention_days,
        ) {
            restore_and_fail!("failed to update backup config", e);
        }

    // Update [agent_tool] section (multi-agent timeouts)
    if (payload.agent_tool_message_wait_for_idle_secs.is_some()
        || payload.agent_tool_message_result_secs.is_some()
        || payload.agent_tool_safety_timeout_secs.is_some())
        && let Err(e) = crate::config::update_agent_tool_config(
            "config/hydeclaw.toml",
            payload.agent_tool_message_wait_for_idle_secs,
            payload.agent_tool_message_result_secs,
            payload.agent_tool_safety_timeout_secs,
        )
    {
        restore_and_fail!("failed to update agent_tool config", e);
    }

    // Validate the written config can be fully deserialized before proceeding
    if let Err(e) = crate::config::AppConfig::load(config_path) {
        // Restore backup — config is broken
        if let Err(restore_err) = tokio::fs::write(config_path, &config_backup).await {
            tracing::error!(error = %e, restore_error = %restore_err, "config validation failed AND backup restore failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": format!("config broken AND restore failed: {}. Manual fix required.", restore_err)
            }))).into_response();
        }
        tracing::error!(error = %e, "config validation failed after update, restored backup");
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("config validation failed, restored backup: {}", e)
        }))).into_response();
    }

    // Update live AgentDeps
    {
        let mut deps = agents.deps.write().await;
        if let Some(ref url) = payload.toolgate_url {
            deps.toolgate_url = if url.is_empty() { None } else { Some(url.clone()) };
        }
    }

    // Reload shared config from file (hot-reload)
    match crate::config::AppConfig::load("config/hydeclaw.toml") {
        Ok(new_config) => {
            new_config.agent_tool.warn_if_invariant_violated();
            let mut config = cfg_svc.shared_config.write().await;
            *config = new_config;
        }
        Err(e) => {
            tracing::warn!(error = %e, "config file updated but failed to reload into memory");
        }
    }

    // Re-initialize memory store if embedding config changed
    if payload.embed_enabled.is_some() || payload.embed_dim.is_some() || payload.embed_dimensions.is_some() {
        tracing::info!("memory config updated — restart required to apply changes");
    }

    tracing::info!("config updated via API");
    crate::db::audit::audit_spawn(infra.db.clone(), String::new(), crate::db::audit::event_types::CONFIG_UPDATED, None, json!({"source": "api"}));
    Json(json!({"ok": true})).into_response()
}

// ── Restart API ──

pub(crate) async fn api_restart(req: Request<Body>) -> impl IntoResponse {
    // Require explicit confirmation header to prevent accidental restarts
    let confirmed = req.headers()
        .get("X-Confirm-Restart")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "true");
    if !confirmed {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "missing X-Confirm-Restart: true header"}))).into_response();
    }

    let ip = crate::gateway::middleware::extract_client_ip(&req);
    tracing::warn!(ip = %ip, "AUDIT: restart confirmed via API");
    // Spawn a delayed exit so the response can be sent first
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        std::process::exit(1);
    });
    Json(json!({"ok": true, "message": "restarting..."})).into_response()
}

// ── Config Export/Import ──

/// Replace the single `hydeclaw.toml.bak` file with timestamped rotation.
///
/// Creates `{config_path}.bak.{YYYY-MM-DDTHH-MM-SSZ}` and keeps the newest
/// `CONFIG_BACKUP_MAX` backups. Older backups are silently deleted.
/// The legacy `.bak` file (no timestamp) is never touched.
const CONFIG_BACKUP_MAX: usize = 5;

async fn rotate_config_backups(config_path: &str) {
    let now = chrono::Utc::now();
    let stamp = now.format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let backup_path = format!("{config_path}.bak.{stamp}");

    // Write the new timestamped backup
    if let Err(e) = tokio::fs::copy(config_path, &backup_path).await {
        tracing::warn!(error = %e, "failed to write config backup, skipping rotation");
        return;
    }

    // Collect existing timestamped backup files (prefix: "hydeclaw.toml.bak.")
    let base = std::path::Path::new(config_path);
    let dir = base.parent().unwrap_or(std::path::Path::new("."));
    let stem = base
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let prefix = format!("{stem}.bak.");

    let mut backups: Vec<std::path::PathBuf> = Vec::new();
    match tokio::fs::read_dir(dir).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                // Match ONLY the timestamped pattern: "hydeclaw.toml.bak.YYYY-..."
                // This avoids deleting the legacy "hydeclaw.toml.bak" file (no trailing dot/timestamp)
                if name.starts_with(&prefix) {
                    backups.push(entry.path());
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to read config dir for backup rotation");
            return;
        }
    }

    // Sort lexicographically — ISO timestamps sort correctly (newest = last alphabetically)
    backups.sort();
    backups.reverse(); // newest first

    // Remove oldest beyond CONFIG_BACKUP_MAX
    for old_path in backups.into_iter().skip(CONFIG_BACKUP_MAX) {
        if let Err(e) = tokio::fs::remove_file(&old_path).await {
            tracing::warn!(error = %e, path = %old_path.display(), "failed to prune old config backup");
        } else {
            tracing::info!(path = %old_path.display(), "pruned old config backup");
        }
    }
}

/// GET /api/config/export — export raw TOML configs (app + all agents).
pub(crate) async fn api_export_config(req: Request<Body>) -> impl IntoResponse {
    let ip = crate::gateway::middleware::extract_client_ip(&req);
    tracing::warn!(ip = %ip, "AUDIT: config export requested");
    let app_toml = std::fs::read_to_string("config/hydeclaw.toml").unwrap_or_default();
    let mut agents = serde_json::Map::new();
    if let Ok(entries) = std::fs::read_dir("config/agents") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                agents.insert(name, Value::String(content));
            }
        }
    }
    Json(json!({
        "app_config": app_toml,
        "agents": agents,
    }))
}

/// POST /api/config/import — import TOML configs (validates before writing, backs up current).
pub(crate) async fn api_import_config(
    State(cfg_svc): State<ConfigServices>,
    req: Request<Body>,
) -> impl IntoResponse {
    let _lock = cfg_svc.config_write_lock.lock().await;
    let ip = crate::gateway::middleware::extract_client_ip(&req);
    tracing::warn!(ip = %ip, "AUDIT: config import requested");
    let body: Value = match axum::Json::<Value>::from_request(req, &()).await {
        Ok(axum::Json(v)) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response(),
    };
    // Validate app config TOML — full semantic validation, not just syntax
    if let Some(app_toml) = body.get("app_config").and_then(|v| v.as_str()) {
        match toml::from_str::<crate::config::AppConfig>(app_toml) {
            Ok(parsed) => {
                // Refuse import that removes auth token (would make gateway unauthenticated)
                if parsed.gateway.auth_token_env.is_none() {
                    return (StatusCode::BAD_REQUEST, Json(json!({
                        "error": "imported config must have gateway.auth_token_env set"
                    }))).into_response();
                }
            }
            Err(e) => {
                return (StatusCode::BAD_REQUEST, Json(json!({
                    "error": format!("invalid app_config: {e}")
                }))).into_response();
            }
        }
        // Backup current (timestamped rotation, keeps newest CONFIG_BACKUP_MAX)
        rotate_config_backups("config/hydeclaw.toml").await;
        if let Err(e) = std::fs::write("config/hydeclaw.toml", app_toml) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    }

    // Import agent configs
    if let Some(agents) = body.get("agents").and_then(|v| v.as_object()) {
        let _ = std::fs::create_dir_all("config/agents");
        for (name, content) in agents {
            // Sanitize name to prevent path traversal
            if name.contains('/') || name.contains('\\') || name.contains("..") || name.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid agent name '{name}'")}))).into_response();
            }
            if let Some(toml_str) = content.as_str() {
                if toml_str.parse::<toml::Table>().is_err() {
                    return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid TOML for agent '{name}'")}))).into_response();
                }
                let path = format!("config/agents/{name}.toml");
                let _ = std::fs::copy(&path, format!("{path}.bak"));
                if let Err(e) = std::fs::write(&path, toml_str) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
                }
            }
        }
    }

    Json(json!({"ok": true})).into_response()
}
