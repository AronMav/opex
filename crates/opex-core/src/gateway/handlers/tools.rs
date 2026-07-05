use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, put},
};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, InfraServices};

include!("tools_dto_structs.rs");

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tool-definitions", get(api_tool_definitions))
        .route("/api/tools", get(api_list_tools))
        .route("/api/tools/import-openapi", post(api_import_openapi))
        .route("/api/mcp", get(api_list_mcp).post(api_mcp_create))
        .route("/api/mcp/{name}", put(api_mcp_update).delete(api_mcp_delete))
        .route("/api/mcp/{name}/reload", post(api_mcp_reload))
        .route("/api/mcp/{name}/toggle", post(api_mcp_toggle))
}

// ── Tools & Skills API ──

/// GET /api/tool-definitions — all tool names available in the system (system + YAML + MCP).
/// Used by agent tool policy UI to show individual tool names for allow/deny.
pub(crate) async fn api_tool_definitions(State(agents): State<AgentCore>) -> Json<Value> {
    use std::collections::BTreeSet;

    let mut names = BTreeSet::new();

    // 1. System (internal) tools — static list
    for &name in crate::agent::engine::all_system_tool_names() {
        names.insert(name.to_string());
    }

    // 2. YAML tools from workspace
    let yaml_tools = crate::tools::yaml_tools::load_yaml_tools(
        crate::config::WORKSPACE_DIR, true, // include draft
    ).await;
    for tool in &yaml_tools {
        names.insert(tool.name.clone());
    }

    // 3. MCP tools (if MCP registry is available)
    let deps = agents.deps.read().await;
    if let Some(ref mcp) = deps.mcp {
        let mcp_tools = mcp.all_tool_definitions().await;
        for tool in &mcp_tools {
            names.insert(tool.name.clone());
        }
    }

    let sorted: Vec<&str> = names.iter().map(std::string::String::as_str).collect();
    Json(json!({ "tools": sorted }))
}

/// POST /api/tools/import-openapi — operator-facing OpenAPI/Swagger import.
/// Fetches the spec (SSRF-safe), writes one draft YAML tool per operation to
/// `workspace/tools/draft/`, and returns what was created. Mirrors the
/// `tool_meta` agent tool but reachable from the Tools UI.
pub(crate) async fn api_import_openapi(Json(body): Json<Value>) -> impl IntoResponse {
    let spec_url = body.get("spec_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if spec_url.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "spec_url is required"}))).into_response();
    }
    let prefix = body.get("prefix").and_then(|v| v.as_str()).unwrap_or("");

    let client = crate::net::ssrf::ssrf_http_client(std::time::Duration::from_secs(20));
    match crate::agent::pipeline::handlers::import_openapi_tools(
        crate::config::WORKSPACE_DIR,
        &client,
        &spec_url,
        prefix,
    )
    .await
    {
        Ok(r) => Json(json!({
            "ok": true,
            "discovered": r.discovered,
            "created": r.created,
            "errors": r.errors,
            "base_url": r.base_url,
        }))
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    }
}

pub(crate) async fn api_list_tools(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
) -> Json<Value> {
    let tool_values = agents.tools.entries().await;
    let managed: Vec<String> = infra.process_manager
        .as_ref()
        .map(|pm| pm.names())
        .unwrap_or_default();
    let tools: Vec<ToolEntryDto> = tool_values.into_iter().filter_map(|mut t| {
        let is_managed = t.get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| managed.iter().any(|m| m == n));
        t["managed"] = json!(is_managed);
        serde_json::from_value(t).map_err(|e| {
            tracing::warn!("tool entry deserialize failed: {e}");
            e
        }).ok()
    }).collect();
    Json(json!({ "tools": tools }))
}

pub(crate) async fn api_list_mcp(State(agents): State<AgentCore>) -> Json<Value> {
    let file_entries = crate::tools::mcp_workspace::load_mcp_entries(crate::config::MCP_DIR).await;

    // Get tool counts from MCP registry cache
    let tool_counts: HashMap<String, usize> = if let Some(ref deps) = {
        let deps = agents.deps.read().await;
        deps.mcp.clone()
    } {
        let cache = deps.all_tool_definitions().await;
        // cache is a flat list; count per MCP is not directly available here,
        // so we leave counts as null (refreshed after discover)
        drop(cache);
        HashMap::new()
    } else {
        HashMap::new()
    };

    let entries: Vec<McpEntryDto> = file_entries.iter().map(|e| {
        McpEntryDto {
            name: e.name.clone(),
            url: e.url.clone(),
            container: e.container.clone(),
            port: e.port,
            mode: e.mode.clone(),
            protocol: e.protocol.clone(),
            enabled: e.enabled,
            status: None,
            tool_count: tool_counts.get(&e.name).copied(),
        }
    }).collect();

    Json(json!({ "mcp": entries }))
}

/// POST /api/mcp — create a new MCP server config.
pub(crate) async fn api_mcp_create(
    State(infra): State<InfraServices>,
    Json(entry): Json<crate::config::McpFileEntry>,
) -> impl IntoResponse {
    use crate::tools::mcp_workspace::save_mcp_entry;
    if let Err(e) = save_mcp_entry(crate::config::MCP_DIR, &entry).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    let cfg = entry.to_config();
    if let Some(ref cm) = infra.container_manager {
        cm.add_or_update_mcp(entry.name.clone(), cfg).await;
        if entry.mode == "persistent"
            && let Err(e) = cm.ensure_running(&entry.name).await {
                tracing::warn!(mcp = %entry.name, error = %e, "failed to start persistent MCP after create");
            }
    }
    tracing::info!(mcp = %entry.name, "MCP server created via UI");
    Json(json!({"ok": true})).into_response()
}

/// PUT /api/mcp/:name — update an existing MCP server config.
pub(crate) async fn api_mcp_update(
    State(infra): State<InfraServices>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(mut entry): Json<crate::config::McpFileEntry>,
) -> impl IntoResponse {
    entry.name = name.clone();
    // Preserve enabled state from existing config — the UI doesn't send `enabled`,
    // so serde defaults it to `true`, which would reset a disabled server on every edit.
    // The toggle endpoint (/api/mcp/{name}/toggle) handles enabled changes separately.
    {
        let path = std::path::Path::new(crate::config::MCP_DIR).join(format!("{name}.yaml"));
        if let Ok(existing_content) = tokio::fs::read_to_string(&path).await
            && let Ok(existing) = serde_yaml::from_str::<crate::config::McpFileEntry>(&existing_content)
        {
            entry.enabled = existing.enabled;
        }
    }
    use crate::tools::mcp_workspace::save_mcp_entry;
    if let Err(e) = save_mcp_entry(crate::config::MCP_DIR, &entry).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    let cfg = entry.to_config();
    if let Some(ref cm) = infra.container_manager {
        cm.add_or_update_mcp(name.clone(), cfg).await;
    }
    tracing::info!(mcp = %name, "MCP server updated via UI");
    Json(json!({"ok": true})).into_response()
}

/// DELETE /api/mcp/:name — delete an MCP server config.
pub(crate) async fn api_mcp_delete(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    use crate::tools::mcp_workspace::delete_mcp_entry;

    if let Some(ref cm) = infra.container_manager {
        // Stop container if running (best effort)
        let _ = cm.stop(&name).await;
        cm.remove_mcp(&name).await;
    }
    // Invalidate tool cache
    {
        let deps = agents.deps.read().await;
        if let Some(ref mcp) = deps.mcp {
            mcp.invalidate_mcp_cache(&name).await;
        }
    }

    match delete_mcp_entry(crate::config::MCP_DIR, &name).await {
        Ok(true) => {
            tracing::info!(mcp = %name, "MCP server deleted via UI");
            Json(json!({"ok": true})).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// POST /api/mcp/:name/reload — invalidate tool cache and rediscover tools.
pub(crate) async fn api_mcp_reload(
    State(agents): State<AgentCore>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let deps = agents.deps.read().await;
    let Some(ref mcp) = deps.mcp else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "MCP not available"}))).into_response();
    };
    match mcp.reload_mcp(&name).await {
        Ok(tools) => {
            tracing::info!(mcp = %name, tools = tools.len(), "MCP reloaded via UI");
            Json(json!({"ok": true, "tool_count": tools.len()})).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// POST /api/mcp/:name/toggle — flip enabled/disabled in YAML config.
pub(crate) async fn api_mcp_toggle(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    use crate::tools::mcp_workspace::{load_mcp_entries, save_mcp_entry};
    let entries = load_mcp_entries(crate::config::MCP_DIR).await;
    let mut entry = match entries.into_iter().find(|e| e.name == name) {
        Some(e) => e,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error": "MCP not found"}))).into_response(),
    };
    entry.enabled = !entry.enabled;
    if let Err(e) = save_mcp_entry(crate::config::MCP_DIR, &entry).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    // Update ContainerManager — add/remove from runtime
    if let Some(ref cm) = infra.container_manager {
        if entry.enabled {
            cm.add_or_update_mcp(entry.name.clone(), entry.to_config()).await;
            if entry.mode == "persistent" {
                cm.ensure_running(&entry.name).await.ok();
            }
        } else {
            cm.remove_mcp(&entry.name).await;
        }
    }
    // Bug 22: when disabling an MCP, also evict its tool definitions from the
    // McpRegistry cache so the LLM stops seeing those tools immediately.
    if !entry.enabled {
        let deps = agents.deps.read().await;
        if let Some(ref mcp) = deps.mcp {
            mcp.invalidate_mcp_cache(&name).await;
        }
    }
    tracing::info!(mcp = %name, enabled = entry.enabled, "MCP toggled via UI");
    Json(json!({"ok": true, "enabled": entry.enabled})).into_response()
}
