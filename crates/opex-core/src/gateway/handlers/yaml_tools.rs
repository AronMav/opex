use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::InfraServices;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/yaml-tools", get(api_yaml_tools_list_global).post(api_yaml_tool_create_global))
        .route("/api/yaml-tools/{tool}/verify", post(api_yaml_tool_verify_global))
        .route("/api/yaml-tools/{tool}/disable", post(api_yaml_tool_disable_global))
        .route("/api/yaml-tools/{tool}/enable", post(api_yaml_tool_enable_global))
        .route("/api/yaml-tools/{tool}", get(api_yaml_tool_get_global).put(api_yaml_tool_update_global).delete(api_yaml_tool_delete_global))
        .route("/api/agents/{name}/yaml-tools", get(api_yaml_tools_list))
        .route("/api/agents/{name}/yaml-tools/{tool}/verify", post(api_yaml_tool_verify))
        .route("/api/agents/{name}/yaml-tools/{tool}/disable", post(api_yaml_tool_disable))
}

/// GET /api/yaml-tools — global, not per-agent.
pub(crate) async fn api_yaml_tools_list_global(State(_state): State<InfraServices>) -> impl IntoResponse {
    let tools = crate::tools::yaml_tools::load_all_yaml_tools(crate::config::WORKSPACE_DIR).await;
    let list: Vec<Value> = tools.iter().map(|t| json!({
        "name": t.name,
        "description": t.description,
        "endpoint": t.endpoint,
        "method": t.method,
        "status": format!("{:?}", t.status).to_lowercase(),
        "parameters_count": t.parameters.len(),
        "tags": t.tags,
    })).collect();
    Json(json!({ "tools": list }))
}

/// POST /api/yaml-tools — create a new YAML tool.
pub(crate) async fn api_yaml_tool_create_global(
    State(_state): State<InfraServices>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    use crate::tools::yaml_tools::{ToolStatus, YamlToolDef, tool_file_path};
    let content = match body.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "missing 'content' field"}))).into_response(),
    };
    // Parse as Value first to check for extends (relaxed validation for template-based tools)
    let yaml_value = match serde_yaml::from_str::<serde_json::Value>(content) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid YAML: {}", e)}))).into_response(),
    };
    let has_extends = yaml_value.get("extends").and_then(|v| v.as_str()).is_some();
    let tool_name = match yaml_value.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "tool must have a 'name' field"}))).into_response(),
    };
    if !has_extends {
        // Full validation only for non-template tools
        let _parsed: YamlToolDef = match serde_yaml::from_str(content) {
            Ok(t) => t,
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid YAML: {}", e)}))).into_response(),
        };
    }
    // Prevent path traversal — tool names must be safe filesystem identifiers
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "tool name must contain only alphanumeric characters, hyphens, and underscores"}))).into_response();
    }
    let parsed_name = tool_name;
    let path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &parsed_name);
    if path.exists() {
        return (StatusCode::CONFLICT, Json(json!({"error": format!("tool '{}' already exists", parsed_name)}))).into_response();
    }
    if let Err(e) = tokio::fs::write(&path, content).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tracing::info!(tool = %parsed_name, "YAML tool created via UI");
    (StatusCode::CREATED, Json(json!({"ok": true, "name": parsed_name}))).into_response()
}

/// POST /api/yaml-tools/{tool}/verify — global.
pub(crate) async fn api_yaml_tool_verify_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(tool_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};
    let path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found"}))).into_response();
    }
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    if content.contains("status: verified") {
        return (StatusCode::CONFLICT, Json(json!({"error": "already verified"}))).into_response();
    }
    let updated = content.replace("status: draft", "status: verified");
    if let Err(e) = tokio::fs::write(&path, &updated).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tracing::info!(tool = %tool_name, "YAML tool verified via UI");
    crate::db::audit::audit_spawn(_state.db.clone(), String::new(), crate::db::audit::event_types::TOOL_VERIFIED, None, json!({"tool": tool_name}));
    Json(json!({"ok": true, "status": "verified"})).into_response()
}

/// POST /api/yaml-tools/{tool}/disable — global.
pub(crate) async fn api_yaml_tool_disable_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(tool_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};
    let path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found"}))).into_response();
    }
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let updated = content
        .replace("status: verified", "status: disabled")
        .replace("status: draft", "status: disabled");
    if let Err(e) = tokio::fs::write(&path, &updated).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tracing::info!(tool = %tool_name, "YAML tool disabled via UI");
    crate::db::audit::audit_spawn(_state.db.clone(), String::new(), crate::db::audit::event_types::TOOL_DISABLED, None, json!({"tool": tool_name}));
    Json(json!({"ok": true, "status": "disabled"})).into_response()
}

/// POST /api/yaml-tools/{tool}/enable — re-enable a disabled tool (set status back to verified).
pub(crate) async fn api_yaml_tool_enable_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(tool_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};
    let path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found"}))).into_response();
    }
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    if content.contains("status: verified") {
        return (StatusCode::CONFLICT, Json(json!({"error": "already enabled"}))).into_response();
    }
    let updated = content
        .replace("status: disabled", "status: verified")
        .replace("status: draft", "status: verified");
    if let Err(e) = tokio::fs::write(&path, &updated).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tracing::info!(tool = %tool_name, "YAML tool enabled via UI");
    crate::db::audit::audit_spawn(_state.db.clone(), String::new(), crate::db::audit::event_types::TOOL_ENABLED, None, json!({"tool": tool_name}));
    Json(json!({"ok": true, "status": "verified"})).into_response()
}

/// GET /api/yaml-tools/{tool} — return raw YAML content of a tool.
pub(crate) async fn api_yaml_tool_get_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(tool_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};
    let path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found"}))).into_response();
    }
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Json(json!({"name": tool_name, "content": content})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// PUT /api/yaml-tools/{tool} — update a YAML tool by writing raw YAML content.
pub(crate) async fn api_yaml_tool_update_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(tool_name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, YamlToolDef, tool_file_path};
    let content = match body.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "missing 'content' field"}))).into_response(),
    };
    // Parse as Value first to check for extends (relaxed validation for template-based tools)
    let yaml_value = match serde_yaml::from_str::<serde_json::Value>(content) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid YAML: {}", e)}))).into_response(),
    };
    let has_extends = yaml_value.get("extends").and_then(|v| v.as_str()).is_some();
    if !has_extends {
        // Full validation only for non-template tools
        if let Err(e) = serde_yaml::from_str::<YamlToolDef>(content) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid YAML: {}", e)}))).into_response();
        }
    }
    let path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found"}))).into_response();
    }
    if let Err(e) = tokio::fs::write(&path, content).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tracing::info!(tool = %tool_name, "YAML tool updated via UI");
    Json(json!({"ok": true})).into_response()
}

/// DELETE /api/yaml-tools/{tool} — delete a YAML tool file.
pub(crate) async fn api_yaml_tool_delete_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(tool_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};
    let path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found"}))).into_response();
    }
    if let Err(e) = tokio::fs::remove_file(&path).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tracing::info!(tool = %tool_name, "YAML tool deleted via UI");
    Json(json!({"ok": true})).into_response()
}

/// GET /api/agents/{name}/yaml-tools
/// List all YAML tools for an agent with their status (verified/draft/disabled).
pub(crate) async fn api_yaml_tools_list(
    State(_state): State<InfraServices>,
    axum::extract::Path(agent_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let tools = crate::tools::yaml_tools::load_all_yaml_tools(
        crate::config::WORKSPACE_DIR,
    ).await;

    let list: Vec<Value> = tools.iter().map(|t| {
        json!({
            "name": t.name,
            "description": t.description,
            "endpoint": t.endpoint,
            "method": t.method,
            "status": format!("{:?}", t.status).to_lowercase(),
            "parameters_count": t.parameters.len(),
            "tags": t.tags,
        })
    }).collect();

    Json(json!({ "tools": list, "agent": agent_name }))
}

/// POST /api/agents/{name}/yaml-tools/{tool}/verify
/// Promote a draft tool to verified. No Telegram approval required (UI is already authed).
pub(crate) async fn api_yaml_tool_verify(
    State(_state): State<InfraServices>,
    axum::extract::Path((agent_name, tool_name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};

    let draft_path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Draft, &tool_name);
    if !draft_path.exists() {
        let verified = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
        if verified.exists() {
            return (StatusCode::CONFLICT, Json(json!({"error": "already verified"}))).into_response();
        }
        return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found in draft"}))).into_response();
    }

    let content = match tokio::fs::read_to_string(&draft_path).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    let updated = content.replace("status: draft", "status: verified");
    let verified_path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);

    if let Err(e) = tokio::fs::write(&verified_path, &updated).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tokio::fs::remove_file(&draft_path).await.ok();

    tracing::info!(agent = %agent_name, tool = %tool_name, "YAML tool verified via UI");
    Json(json!({"ok": true, "status": "verified"})).into_response()
}

/// POST /api/agents/{name}/yaml-tools/{tool}/disable
/// Move a verified or draft tool to disabled.
pub(crate) async fn api_yaml_tool_disable(
    State(_state): State<InfraServices>,
    axum::extract::Path((agent_name, tool_name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    if !tool_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid tool name"}))).into_response();
    }
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};

    let src_path = {
        let verified = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Verified, &tool_name);
        let draft = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Draft, &tool_name);
        if verified.exists() { verified } else if draft.exists() { draft } else {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "tool not found"}))).into_response();
        }
    };

    let content = match tokio::fs::read_to_string(&src_path).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    let updated = content
        .replace("status: verified", "status: disabled")
        .replace("status: draft", "status: disabled");

    let disabled_path = tool_file_path(crate::config::WORKSPACE_DIR, &ToolStatus::Disabled, &tool_name);
    if let Some(parent) = disabled_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    if let Err(e) = tokio::fs::write(&disabled_path, &updated).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    tokio::fs::remove_file(&src_path).await.ok();

    tracing::info!(agent = %agent_name, tool = %tool_name, "YAML tool disabled via UI");
    Json(json!({"ok": true, "status": "disabled"})).into_response()
}
