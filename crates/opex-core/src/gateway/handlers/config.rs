use axum::{
    Router,
    body::Body,
    extract::{State},
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

pub(crate) async fn api_tts_voices(
    State(agents): State<AgentCore>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let toolgate_url = {
        let deps = agents.deps.read().await;
        deps.toolgate_url.clone()
    };
    let Some(base) = toolgate_url else {
        return Json(json!({"voices": []})).into_response();
    };
    let url = format!("{}/audio/voices", base.trim_end_matches('/'));
    let client = TOOLGATE_CLIENT.get_or_init(reqwest::Client::new);
    // Optional provider override: ?provider=<name> → X-Opex-Provider header.
    // toolgate's require_provider("tts") honors this header and uses the named
    // provider instead of the global active one — letting the UI fetch voice
    // lists for any TTS provider, not only the currently active one.
    let mut req = client.get(&url).timeout(std::time::Duration::from_secs(5));
    if let Some(prov) = params.get("provider").filter(|s| !s.is_empty()) {
        req = req.header("X-Opex-Provider", prov);
    }
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<serde_json::Value>().await {
                Ok(data) => Json(data).into_response(),
                Err(_) => Json(json!({"voices": []})).into_response(),
            }
        }
        _ => Json(json!({"voices": []})).into_response(),
    }
}

/// Voice priority: manual `body.voice` override beats the profile slot's
/// configured voice, which beats the provider's own default (empty string —
/// toolgate/the provider picks it).
fn effective_voice(body_voice: Option<&str>, slot_voice: Option<&str>) -> String {
    body_voice
        .filter(|v| !v.is_empty())
        .or(slot_voice.filter(|v| !v.is_empty()))
        .unwrap_or("")
        .to_string()
}

/// POST /api/tts/synthesize — synthesize speech via Toolgate.
///
/// When `?agent=<name>` is present, resolves the tts capability chain from
/// the agent's profile (`profile_resolver::resolve_slots_for_agent` +
/// `effective_chain`) and retries down the chain on retryable failures
/// (5xx / 429), sending `X-Opex-Provider: <entry.provider>` per attempt so
/// toolgate uses that specific provider instead of the globally active one.
/// An agent whose profile has no `tts` slot gets a `409 tts_disabled` instead
/// of silently falling back to the active provider (contract for spec #2).
/// Without `?agent=`, behavior is legacy: a single request, no provider
/// header, toolgate decides.
pub(crate) async fn api_tts_synthesize(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<TtsSynthesizeRequest>,
) -> impl IntoResponse {
    let toolgate_url = {
        let deps = agents.deps.read().await;
        deps.toolgate_url.clone()
    };
    let Some(base) = toolgate_url else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Toolgate URL not configured").into_response();
    };

    // Chain from the agent's profile (?agent=); no param — legacy behavior
    // (empty chain → single request, no X-Opex-Provider, toolgate decides).
    let chain: Vec<crate::db::profiles::SlotEntry> = match params.get("agent") {
        Some(agent_name) => {
            let profile_name = agents
                .get_engine(agent_name)
                .await
                .map(|e| e.cfg().agent.profile.clone())
                .unwrap_or_else(|| crate::db::profiles::DEFAULT_PROFILE.to_string());
            let slots = crate::agent::profile_resolver::resolve_slots_for_agent(
                &infra.db,
                &profile_name,
                agent_name,
            ).await;
            let chain = crate::agent::profile_resolver::effective_chain(&infra.db, &slots, "tts").await;
            if chain.is_empty() {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error": "tts_disabled", "hint": "profile has no tts slot"})),
                ).into_response();
            }
            chain
        }
        None => Vec::new(),
    };

    let client = TOOLGATE_CLIENT.get_or_init(reqwest::Client::new);
    let attempts: Vec<Option<&crate::db::profiles::SlotEntry>> = if chain.is_empty() {
        vec![None]
    } else {
        chain.iter().map(Some).collect()
    };

    let last = attempts.len() - 1;
    for (i, entry) in attempts.into_iter().enumerate() {
        // Build the body, deferring `model` to the active provider's
        // configured defaults when the caller omits it. Hardcoding "tts-1"
        // here forced the piper tier on OpenAI-style servers (e.g.
        // openedai-speech), where an XTTS voice clone is only reachable via
        // the provider's `default_model` ("tts-1-hd") — so the preview
        // returned empty audio. Omitting the field lets toolgate fall back
        // to that default_model.
        let voice = effective_voice(
            body.voice.as_deref(),
            entry.and_then(|e| e.voice.as_deref()),
        );
        let mut payload = serde_json::json!({ "input": body.text, "voice": voice });
        if let Some(ref model) = body.model
            && !model.is_empty()
        {
            payload["model"] = serde_json::Value::String(model.clone());
        }
        let mut req = client
            .post(format!("{}/v1/audio/speech", base.trim_end_matches('/')))
            .json(&payload);
        if let Some(e) = entry {
            req = req.header("X-Opex-Provider", &e.provider);
        }

        match req.send().await {
            Ok(r) if r.status().is_success() => {
                let content_type = r.headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("audio/mpeg")
                    .to_string();
                let bytes = r.bytes().await.unwrap_or_default();
                return (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, content_type)],
                    bytes,
                ).into_response();
            }
            Ok(r) => {
                let status = r.status();
                let retryable = status.is_server_error() || status.as_u16() == 429;
                if retryable && i < last {
                    continue;
                }
                let text = r.text().await.unwrap_or_default();
                return (
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                    text,
                ).into_response();
            }
            Err(e) => {
                if i < last {
                    continue;
                }
                return (StatusCode::BAD_GATEWAY, format!("Toolgate error: {e}")).into_response();
            }
        }
    }
    // Unreachable: `attempts` always has >=1 entry, and the loop above
    // returns on the final attempt regardless of outcome.
    (StatusCode::BAD_GATEWAY, "no tts attempts made").into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct TtsSynthesizeRequest {
    text: String,
    voice: Option<String>,
    model: Option<String>,
}

#[cfg(test)]
mod tts_tests {
    use super::effective_voice;

    #[test]
    fn voice_priority_body_over_slot_over_default() {
        assert_eq!(effective_voice(Some("manual"), Some("prof")), "manual");
        assert_eq!(effective_voice(None, Some("prof")), "prof");
        assert_eq!(effective_voice(Some(""), None), "");
    }
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
        },
        "subagents": {
            "enabled": config.subagents.enabled,
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
        "curator": {
            "enabled": config.curator.enabled,
            "cron": config.curator.cron,
            "min_idle_minutes": config.curator.min_idle_minutes,
            "stale_after_days": config.curator.stale_after_days,
            "archive_after_days": config.curator.archive_after_days,
            "max_repairs_per_run": config.curator.max_repairs_per_run,
            "agent_name": config.curator.agent_name,
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
    public_url: Option<String>,
    backup_enabled: Option<bool>,
    backup_cron: Option<String>,
    backup_retention_days: Option<u32>,
    // [curator] — skill curator settings.
    curator_enabled: Option<bool>,
    curator_cron: Option<String>,
    curator_min_idle_minutes: Option<u32>,
    curator_stale_after_days: Option<u32>,
    curator_archive_after_days: Option<u32>,
    curator_max_repairs_per_run: Option<u32>,
    curator_agent_name: Option<String>,
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

    let config_path = opex_gateway_util::config_path::resolve_config_path();

    // Serialize config writes to prevent concurrent partial updates
    let _config_guard = cfg_svc.config_write_lock.lock().await;

    // Create backup before modifying — fail if unreadable (don't risk empty restore)
    let config_backup = match tokio::fs::read_to_string(&config_path).await {
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
            if let Err(restore_err) = tokio::fs::write(&config_path, &config_backup).await {
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
        &config_path,
        payload.toolgate_url.as_deref(),
    ) {
        restore_and_fail!("failed to update config file", e);
    }

    // Update memory config in TOML
    if (payload.embed_enabled.is_some() || payload.embed_dim.is_some() || payload.embed_dimensions.is_some())
        && let Err(e) = crate::config::update_memory_config(
            &config_path,
            payload.embed_enabled,
            payload.embed_dim,
            payload.embed_dimensions,
        ) {
            restore_and_fail!("failed to update memory config", e);
        }

    // Update subagents config in TOML
    if let Some(enabled) = payload.subagents_enabled
        && let Err(e) = crate::config::update_subagents_enabled(&config_path, enabled) {
            restore_and_fail!("failed to update subagents config", e);
        }

    // Update limits config in TOML
    if (payload.max_requests_per_minute.is_some() || payload.max_tool_concurrency.is_some())
        && let Err(e) = crate::config::update_limits_config(
            &config_path,
            payload.max_requests_per_minute,
            payload.max_tool_concurrency,
        )
    {
        restore_and_fail!("failed to update limits config", e);
    }

    // Update public_url in TOML
    if let Some(ref url) = payload.public_url
        && let Err(e) = crate::config::update_public_url(&config_path, url)
    {
        restore_and_fail!("failed to update public_url config", e);
    }

    // Update backup config in TOML
    if (payload.backup_enabled.is_some() || payload.backup_cron.is_some() || payload.backup_retention_days.is_some())
        && let Err(e) = crate::config::update_backup_config(
            &config_path,
            payload.backup_enabled,
            payload.backup_cron.as_deref(),
            payload.backup_retention_days,
        ) {
            restore_and_fail!("failed to update backup config", e);
        }

    // Update curator config in TOML
    if (payload.curator_enabled.is_some()
        || payload.curator_cron.is_some()
        || payload.curator_min_idle_minutes.is_some()
        || payload.curator_stale_after_days.is_some()
        || payload.curator_archive_after_days.is_some()
        || payload.curator_max_repairs_per_run.is_some()
        || payload.curator_agent_name.is_some())
        && let Err(e) = crate::config::update_curator_config(
            &config_path,
            payload.curator_enabled,
            payload.curator_cron.as_deref(),
            payload.curator_min_idle_minutes,
            payload.curator_stale_after_days,
            payload.curator_archive_after_days,
            payload.curator_max_repairs_per_run,
            payload.curator_agent_name.as_deref(),
        )
    {
        restore_and_fail!("failed to update curator config", e);
    }

    // Update [agent_tool] section (multi-agent timeouts)
    if (payload.agent_tool_message_wait_for_idle_secs.is_some()
        || payload.agent_tool_message_result_secs.is_some()
        || payload.agent_tool_safety_timeout_secs.is_some())
        && let Err(e) = crate::config::update_agent_tool_config(
            &config_path,
            payload.agent_tool_message_wait_for_idle_secs,
            payload.agent_tool_message_result_secs,
            payload.agent_tool_safety_timeout_secs,
        )
    {
        restore_and_fail!("failed to update agent_tool config", e);
    }

    // Validate the written config can be fully deserialized before proceeding
    if let Err(e) = crate::config::AppConfig::load(&config_path) {
        // Restore backup — config is broken
        if let Err(restore_err) = tokio::fs::write(&config_path, &config_backup).await {
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
    let new_config = match crate::config::AppConfig::load(&config_path) {
        Ok(new_config) => {
            new_config.agent_tool.warn_if_invariant_violated();
            let mut config = cfg_svc.shared_config.write().await;
            *config = new_config.clone();
            Some(new_config)
        }
        Err(e) => {
            tracing::warn!(error = %e, "config file updated but failed to reload into memory");
            None
        }
    };

    // Reschedule backup if its config changed
    if let Some(ref cfg) = new_config {
        if (payload.backup_enabled.is_some() || payload.backup_cron.is_some() || payload.backup_retention_days.is_some())
            && cfg.backup.enabled {
                if let Err(e) = agents.scheduler.reschedule_backup(
                    &cfg.backup.cron,
                    cfg.backup.retention_days,
                    cfg.backup.postgres_container.clone(),
                    infra.secrets.clone(),
                    agents.deps.clone(),
                ).await {
                    tracing::warn!(error = %e, "backup rescheduled with errors");
                } else {
                    tracing::info!(cron = %cfg.backup.cron, "backup rescheduled");
                }
            }
        // Reschedule curator if its config changed
        if payload.curator_enabled.is_some() || payload.curator_cron.is_some() {
            if let Err(e) = agents.scheduler.reschedule_curator(
                cfg.curator.clone(),
                infra.db.clone(),
                agents.clone(),
            ).await {
                tracing::warn!(error = %e, "curator rescheduled with errors");
            } else {
                tracing::info!(cron = %cfg.curator.cron, enabled = cfg.curator.enabled, "curator rescheduled");
            }
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

