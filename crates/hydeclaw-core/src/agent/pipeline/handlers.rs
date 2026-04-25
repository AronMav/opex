//! Pipeline step: handlers — tool result handlers for workspace, browser (migrated from engine_handlers.rs).
//!
//! Each function takes explicit dependencies instead of `&self` on `AgentEngine`.

use std::sync::Arc;

use anyhow::Result;

use crate::agent::workspace;
use crate::secrets::SecretsManager;

// ── Workspace handlers ──────────────────────────────────────────

/// Internal tool: write a workspace file.
pub async fn handle_workspace_write(
    workspace_dir: &str,
    agent_name: &str,
    is_base: bool,
    args: &serde_json::Value,
) -> String {
    let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    // Accept content as string or convert other JSON types to string
    let content = match args.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };

    if filename.is_empty() {
        return "Error: 'filename' is required".to_string();
    }

    match workspace::write_workspace_file(workspace_dir, agent_name, filename, &content, is_base)
        .await
    {
        Ok(()) => format!("Successfully updated {} ({}B)", filename, content.len()),
        Err(e) => {
            tracing::error!(
                filename = %filename,
                workspace = %workspace_dir,
                agent = %agent_name,
                error = %e,
                "workspace_write failed"
            );
            format!("Error writing {}: {}", filename, e)
        }
    }
}

/// Internal tool: read a file from workspace.
pub async fn handle_workspace_read(
    workspace_dir: &str,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("");

    if filename.is_empty() {
        return "Error: 'filename' is required".to_string();
    }

    match workspace::read_workspace_file(workspace_dir, agent_name, filename).await {
        Ok(content) => content,
        Err(e) => format!("Error reading '{}': {}", filename, e),
    }
}

/// Internal tool: list files in workspace directory.
pub async fn handle_workspace_list(
    workspace_dir: &str,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let directory = args
        .get("directory")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    match workspace::list_workspace_files(workspace_dir, agent_name, directory).await {
        Ok(listing) => listing,
        Err(e) => format!("Error listing '{}': {}", directory, e),
    }
}

/// Internal tool: edit a file by replacing a text substring.
pub async fn handle_workspace_edit(
    workspace_dir: &str,
    agent_name: &str,
    is_base: bool,
    args: &serde_json::Value,
) -> String {
    let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    let old_text = args.get("old_text").and_then(|v| v.as_str()).unwrap_or("");
    let new_text = args.get("new_text").and_then(|v| v.as_str()).unwrap_or("");

    if filename.is_empty() || old_text.is_empty() {
        return "Error: 'filename' and 'old_text' are required".to_string();
    }

    match workspace::edit_workspace_file(
        workspace_dir,
        agent_name,
        filename,
        old_text,
        new_text,
        is_base,
    )
    .await
    {
        Ok(()) => format!("Successfully edited '{}'", filename),
        Err(e) => format!("Error editing '{}': {}", filename, e),
    }
}

/// Internal tool: delete a workspace file.
pub async fn handle_workspace_delete(
    workspace_dir: &str,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("");
    if filename.is_empty() {
        return "Error: 'filename' is required".to_string();
    }
    match workspace::delete_workspace_file(workspace_dir, agent_name, filename).await {
        Ok(()) => format!("Deleted '{}'", filename),
        Err(e) => format!("Error deleting '{}': {}", filename, e),
    }
}

/// Internal tool: rename/move a workspace file.
pub async fn handle_workspace_rename(
    workspace_dir: &str,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let old_path = args
        .get("old_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_path = args
        .get("new_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if old_path.is_empty() || new_path.is_empty() {
        return "Error: 'old_path' and 'new_path' are required".to_string();
    }
    match workspace::rename_workspace_file(workspace_dir, agent_name, old_path, new_path).await {
        Ok(()) => format!("Moved '{}' → '{}'", old_path, new_path),
        Err(e) => format!("Error moving '{}': {}", old_path, e),
    }
}

// ── Browser handler ─────────────────────────────────────────────

/// Handle browser automation actions via browser-renderer /automation endpoint.
pub async fn handle_browser_action(
    http_client: &reqwest::Client,
    browser_renderer_url: &str,
    args: &serde_json::Value,
) -> String {
    // SSRF protection: validate URL in navigate actions to block internal services
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if (action == "navigate" || action == "create_session")
        && let Some(url) = args.get("url").and_then(|v| v.as_str())
        && let Err(e) = crate::tools::ssrf::validate_url_scheme(url)
    {
        return format!("Error: {e}");
    }
    match br_post(http_client, browser_renderer_url, "/automation", args.clone()).await {
        Ok(result) => {
            serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
        }
        Err(e) => format!("Error: {e}"),
    }
}

/// POST to browser-renderer at the given base URL + path.
async fn br_post(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("browser-renderer request failed: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("failed to read browser-renderer response: {e}"))?;
    if !status.is_success() {
        return Err(format!("browser-renderer {status}: {text}"));
    }
    serde_json::from_str(&text)
        .map_err(|e| format!("invalid JSON from browser-renderer: {e} — raw: {text}"))
}

// ── Media helpers ───────────────────────────────────────────────

/// Save binary data to workspace/uploads/ and return (signed_url, media_type).
///
/// Phase 64 SEC-03: the returned URL is HMAC-signed with a TTL so agent-emitted
/// media (Telegram send_photo, send_voice) cannot be enumerated or replayed
/// indefinitely. `upload_key` is the HKDF-derived per-domain key obtained via
/// `SecretsManager::get_upload_hmac_key()`; callers MUST NOT pass raw master
/// bytes here. `ttl_secs` is sourced from `[uploads] signed_url_ttl_secs` in
/// hydeclaw.toml (default 24 h).
///
/// The URL is relative (`/uploads/{uuid}.{ext}?sig=…&exp=…`) so the UI and
/// channel adapters append it to their own base. GET /uploads verifies the
/// signature; see `crate::gateway::handlers::media::api_media_serve`.
pub async fn save_binary_to_uploads(
    workspace_dir: &str,
    data: &[u8],
    hint: &str,
    upload_key: &[u8; 32],
    ttl_secs: u64,
) -> Result<(String, String)> {
    let uploads_dir = std::path::PathBuf::from(workspace_dir).join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await?;

    // Detect image type from magic bytes
    let (ext, media_type) = detect_media_type(data, hint);
    let uuid = uuid::Uuid::new_v4();
    let filename = format!("{}.{}", uuid, ext);
    let path = uploads_dir.join(&filename);

    tokio::fs::write(&path, data).await?;

    // Phase 64 SEC-03: mint signed URL. Empty base → relative "/uploads/{file}?...".
    let url = crate::uploads::mint_signed_url("", &filename, upload_key, ttl_secs);
    tracing::info!(url = %url, media_type = %media_type, bytes = data.len(), "saved signed media to uploads");
    Ok((url, media_type))
}

/// Detect media type from magic bytes, returning (extension, mime_type).
pub fn detect_media_type(data: &[u8], hint: &str) -> (&'static str, String) {
    // Check magic bytes
    if data.len() >= 8 {
        if data.starts_with(b"\x89PNG") {
            return ("png", "image/png".into());
        }
        if data.starts_with(b"\xFF\xD8\xFF") {
            return ("jpg", "image/jpeg".into());
        }
        if data.starts_with(b"GIF8") {
            return ("gif", "image/gif".into());
        }
        if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            return ("webp", "image/webp".into());
        }
        if data.starts_with(b"OggS") {
            return ("ogg", "audio/ogg".into());
        }
    }
    // Fallback based on hint
    match hint {
        "image" => ("png", "image/png".into()),
        "audio" => ("ogg", "audio/ogg".into()),
        _ => ("bin", "application/octet-stream".into()),
    }
}

// ── Tool management handlers ───────────────────────────────────

/// Internal tool: create a new YAML HTTP tool in draft status.
pub async fn handle_tool_create(workspace_dir: &str, args: &serde_json::Value) -> String {
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};

    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return "Error: 'name' is required".to_string(),
    };

    let valid = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.starts_with(|c: char| c.is_ascii_lowercase());
    if !valid {
        return "Error: tool name must be snake_case (lowercase letters, digits, underscores, starting with a letter)".to_string();
    }

    let description = args.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let endpoint = match args.get("endpoint").and_then(|v| v.as_str()) {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => return "Error: 'endpoint' is required".to_string(),
    };
    let method = args.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_uppercase();

    let mut yaml_parts = vec![
        format!("name: {}", name),
        format!("description: {:?}", description),
        format!("endpoint: {:?}", endpoint),
        format!("method: {}", method),
        "status: draft".to_string(),
        format!("created_by: agent"),
        format!("created_at: {:?}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
    ];

    if let Some(tags) = args.get("tags").and_then(|v| v.as_array()) {
        let tag_list: Vec<String> = tags.iter()
            .filter_map(|t| t.as_str().map(|s| format!("  - {}", s)))
            .collect();
        if !tag_list.is_empty() {
            yaml_parts.push(format!("tags:\n{}", tag_list.join("\n")));
        }
    }

    if let Some(auth) = args.get("auth") {
        match serde_yaml::to_string(auth) {
            Ok(auth_yaml) => {
                let indented = auth_yaml.lines()
                    .map(|l| format!("  {}", l))
                    .collect::<Vec<_>>()
                    .join("\n");
                yaml_parts.push(format!("auth:\n{}", indented));
            }
            Err(e) => return format!("Error serializing auth: {}", e),
        }
    }

    if let Some(headers) = args.get("headers")
        && let Ok(h_yaml) = serde_yaml::to_string(headers) {
            let indented = h_yaml.lines()
                .map(|l| format!("  {}", l))
                .collect::<Vec<_>>()
                .join("\n");
            yaml_parts.push(format!("headers:\n{}", indented));
        }

    if let Some(params) = args.get("parameters") {
        match serde_yaml::to_string(params) {
            Ok(p_yaml) => {
                let indented = p_yaml.lines()
                    .map(|l| format!("  {}", l))
                    .collect::<Vec<_>>()
                    .join("\n");
                yaml_parts.push(format!("parameters:\n{}", indented));
            }
            Err(e) => return format!("Error serializing parameters: {}", e),
        }
    }

    if let Some(tmpl) = args.get("body_template").and_then(|v| v.as_str()) {
        yaml_parts.push(format!("body_template: |\n{}", tmpl.lines().map(|l| format!("  {}", l)).collect::<Vec<_>>().join("\n")));
    }

    let yaml_content = yaml_parts.join("\n") + "\n";

    if let Err(e) = serde_yaml::from_str::<crate::tools::yaml_tools::YamlToolDef>(&yaml_content) { return format!("Error: generated YAML is invalid: {}\n\nYAML:\n{}", e, yaml_content) }

    let path = tool_file_path(workspace_dir, &ToolStatus::Draft, &name);
    if let Some(parent) = path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await {
            return format!("Error creating directory: {}", e);
        }
    match tokio::fs::write(&path, &yaml_content).await {
        Ok(_) => format!(
            "Tool '{}' created in DRAFT status.\nFile: tools/{}.yaml\n\nNext steps:\n1. Test it: tool_test(tool_name=\"{}\", params={{...}})\n2. Verify it: tool_verify(tool_name=\"{}\")",
            name, name, name, name
        ),
        Err(e) => format!("Error writing tool file: {}", e),
    }
}

/// Internal tool: list YAML tools by status.
pub async fn handle_tool_list(workspace_dir: &str, args: &serde_json::Value) -> String {
    use crate::tools::yaml_tools::{load_all_yaml_tools, ToolStatus};

    let status_filter = args.get("status").and_then(|v| v.as_str()).unwrap_or("all");

    let all_tools = load_all_yaml_tools(workspace_dir).await;

    let tools: Vec<_> = all_tools.iter().filter(|t| {
        match status_filter {
            "verified" => t.status == ToolStatus::Verified,
            "draft" => t.status == ToolStatus::Draft,
            "disabled" => t.status == ToolStatus::Disabled,
            _ => true,
        }
    }).collect();

    if tools.is_empty() {
        return format!("No {} tools found.", status_filter);
    }

    let lines: Vec<String> = tools.iter().map(|t| {
        let status_icon = match t.status {
            ToolStatus::Verified => "✅",
            ToolStatus::Draft => "✏️",
            ToolStatus::Disabled => "🚫",
        };
        format!("{} **{}** — {}\n   `{} {}`",
            status_icon, t.name, t.description, t.method, t.endpoint)
    }).collect();

    format!("**YAML Tools** ({} {}):\n\n{}", tools.len(), status_filter, lines.join("\n\n"))
}

/// Internal tool: test a YAML tool (including draft) with specific parameters.
pub async fn handle_tool_test(
    workspace_dir: &str,
    http_client: &reqwest::Client,
    ssrf_client: &reqwest::Client,
    secrets: &Arc<SecretsManager>,
    agent_name: &str,
    oauth: Option<&Arc<crate::oauth::OAuthManager>>,
    args: &serde_json::Value,
) -> String {
    use super::context::{make_resolver, make_oauth_context};

    let tool_name = match args.get("tool_name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return "Error: 'tool_name' is required".to_string(),
    };
    let params = args.get("params").cloned().unwrap_or(serde_json::Value::Object(Default::default()));
    let dry_run = args.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);

    let tool = match crate::tools::yaml_tools::find_yaml_tool(
        workspace_dir, tool_name,
    ).await {
        Some(t) => t,
        None => return format!("Tool '{}' not found. Use tool_list() to see available tools.", tool_name),
    };

    if dry_run {
        return format!(
            "**Dry run for '{}'** (status: {:?})\n\nEndpoint: {} {}\nAuth: {:?}\nParameters: {}\n\nWould send params: {}",
            tool.name,
            tool.status,
            tool.method,
            tool.endpoint,
            tool.auth.as_ref().map(|a| &a.auth_type),
            serde_json::to_string_pretty(&tool.parameters.keys().collect::<Vec<_>>()).unwrap_or_default(),
            serde_json::to_string_pretty(&params).unwrap_or_default(),
        );
    }

    let resolver = make_resolver(secrets, agent_name);
    let oauth_ctx = make_oauth_context(oauth, agent_name);
    let start = std::time::Instant::now();
    // Internal endpoints (toolgate, searxng, etc.) bypass SSRF filtering
    let client = if crate::tools::ssrf::is_internal_endpoint(&tool.endpoint) {
        http_client
    } else {
        ssrf_client
    };
    let result = tool.execute_oauth(&params, client, Some(&resolver), oauth_ctx.as_ref()).await;
    let elapsed_ms = start.elapsed().as_millis();

    match result {
        Ok(body) => format!(
            "**tool_test('{}')** ✅ ({} ms)\n\nResponse:\n```\n{}\n```",
            tool_name,
            elapsed_ms,
            if body.len() > 2000 { &body[..body.floor_char_boundary(2000)] } else { &body },
        ),
        Err(e) => format!(
            "**tool_test('{}')** ❌ ({} ms)\n\nError: {}",
            tool_name, elapsed_ms, e
        ),
    }
}

/// Internal tool: promote a draft tool to verified status.
pub async fn handle_tool_verify(workspace_dir: &str, args: &serde_json::Value) -> String {
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};
    use regex::Regex;

    let tool_name = match args.get("tool_name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return "Error: 'tool_name' is required".to_string(),
    };

    let path = tool_file_path(workspace_dir, &ToolStatus::Draft, tool_name);
    if !path.exists() {
        return format!("Tool '{}' not found. Use tool_list(status=\"draft\") to see draft tools.", tool_name);
    }

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return format!("Error reading tool file: {}", e),
    };

    let status_re = Regex::new(r"(?m)^status:\s*verified\s*$").unwrap();
    if status_re.is_match(&content) {
        return format!("Tool '{}' is already verified.", tool_name);
    }

    let draft_re = Regex::new(r"(?m)^status:\s*draft\s*$").unwrap();
    let updated = draft_re.replace(&content, "status: verified").to_string();
    if let Err(e) = tokio::fs::write(&path, &updated).await {
        return format!("Error writing tool file: {}", e);
    }

    format!(
        "Tool '{}' is now VERIFIED ✅\nIt will appear in LLM context on next request.",
        tool_name
    )
}

/// Internal tool: move a tool to disabled status.
pub async fn handle_tool_disable(workspace_dir: &str, args: &serde_json::Value) -> String {
    use crate::tools::yaml_tools::{ToolStatus, tool_file_path};
    use regex::Regex;

    let tool_name = match args.get("tool_name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return "Error: 'tool_name' is required".to_string(),
    };

    // Check both verified and draft paths (tool could be in either status)
    let verified_path = tool_file_path(workspace_dir, &ToolStatus::Verified, tool_name);
    let draft_path = tool_file_path(workspace_dir, &ToolStatus::Draft, tool_name);
    let path = if verified_path.exists() {
        verified_path
    } else if draft_path.exists() {
        draft_path
    } else {
        return format!("Tool '{}' not found.", tool_name);
    };

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return format!("Error reading tool file: {}", e),
    };

    let status_re = Regex::new(r"(?m)^status:\s*(verified|draft)\s*$").unwrap();
    let updated = status_re.replace(&content, "status: disabled").to_string();

    if let Err(e) = tokio::fs::write(&path, &updated).await {
        return format!("Error writing tool file: {}", e);
    }

    format!("Tool '{}' disabled 🚫\nIt will not appear in LLM context.", tool_name)
}

/// Return a `__rich_card__:` marker so the SSE handler emits a RichCard event inline.
pub fn handle_rich_card(args: &serde_json::Value) -> String {
    use crate::agent::engine::RICH_CARD_PREFIX;

    let card_type = args.get("card_type").and_then(|v| v.as_str()).unwrap_or("table");
    match card_type {
        "table" | "metric" => {}
        other => return format!("Unknown rich_card type: {other}"),
    }
    format!("{RICH_CARD_PREFIX}{}", serde_json::to_string(args).unwrap_or_default())
}

// ── Secret handlers ────────────────────────────────────────────

/// Internal tool: set a secret in the vault.
pub async fn handle_secret_set(
    secrets: &Arc<SecretsManager>,
    agent_name: &str,
    is_base: bool,
    args: &serde_json::Value,
) -> String {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return "Error: 'name' is required".to_string(),
    };
    let value = match args.get("value").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v,
        _ => return "Error: 'value' is required".to_string(),
    };
    let description = args.get("description").and_then(|v| v.as_str());
    let global = args.get("global").and_then(|v| v.as_bool()).unwrap_or(false);

    // Only base agents can set global secrets (prevents credential substitution attacks)
    if global && !is_base {
        return "Error: only base agents can set global secrets. Use scoped secrets or delegate to the base agent.".to_string();
    }

    let result = if global {
        secrets.set(name, value, description).await
    } else {
        secrets.set_scoped(name, agent_name, value, description).await
    };

    match result {
        Ok(()) => {
            let scope_label = if global { "global" } else { agent_name };
            format!("Secret '{}' saved (scope: {}). It is now available for YAML tool auth.", name, scope_label)
        }
        Err(e) => format!("Error saving secret: {}", e),
    }
}

// ── Skill handlers ─────────────────────────────────────────────

/// Skill meta-tool: create a new skill scenario.
pub async fn handle_skill_create(workspace_dir: &str, args: &serde_json::Value) -> String {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return "Error: 'name' is required".to_string(),
    };
    let description = args.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let triggers: Vec<String> = args
        .get("triggers")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let tools_required: Vec<String> = args
        .get("tools_required")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let instructions = match args.get("instructions").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "Error: 'instructions' is required".to_string(),
    };
    let priority = args.get("priority").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

    let frontmatter = crate::skills::SkillFrontmatter {
        name: name.clone(),
        description,
        triggers,
        tools_required,
        priority,
    };

    match crate::skills::write_skill(
        workspace_dir,
        &name,
        &frontmatter,
        &instructions,
    ).await {
        Ok(()) => format!("Skill '{}' created in skills/{}.md", name, name.replace(' ', "-")),
        Err(e) => format!("Error creating skill '{}': {}", name, e),
    }
}

/// Skill use: on-demand skill loading (list catalog or load full instructions).
pub async fn handle_skill_use(
    workspace_dir: &str,
    is_base: bool,
    available_tools: &std::collections::HashSet<String>,
    args: &serde_json::Value,
) -> String {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("list");
    let skills = if is_base {
        crate::skills::load_skills_for_base(workspace_dir).await
    } else {
        crate::skills::load_skills(workspace_dir).await
    };

    match action {
        "list" => {
            let visible = crate::skills::filter_skills_by_available_tools(skills, available_tools);
            if visible.is_empty() {
                return "No skills available.".to_string();
            }
            let mut out = String::from("Available skills:\n\n");
            for s in &visible {
                out.push_str(&format!("- **{}** — {}", s.meta.name, s.meta.description));
                if !s.meta.triggers.is_empty() {
                    out.push_str(&format!(" (use when: {})", s.meta.triggers.join(", ")));
                }
                out.push('\n');
            }
            out
        }
        "load" => {
            // load is NOT filtered — direct references by name must keep working.
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return "Error: 'name' parameter required for load action.".to_string();
            }
            match skills.iter().find(|s| s.meta.name == name) {
                Some(skill) => {
                    format!("## Skill: {}\n{}\n\n{}", skill.meta.name, skill.meta.description, skill.instructions)
                }
                None => {
                    let available: Vec<&str> = skills.iter().map(|s| s.meta.name.as_str()).collect();
                    format!("Skill '{}' not found. Available: {}", name, available.join(", "))
                }
            }
        }
        _ => format!("Error: unknown action '{}'. Use: list, load.", action),
    }
}

/// Skill meta-tool: list available skills, filtered by tools the agent may call.
pub async fn handle_skill_list(
    workspace_dir: &str,
    is_base: bool,
    available_tools: &std::collections::HashSet<String>,
    _args: &serde_json::Value,
) -> String {
    let skills = if is_base {
        crate::skills::load_skills_for_base(workspace_dir).await
    } else {
        crate::skills::load_skills(workspace_dir).await
    };
    let visible = crate::skills::filter_skills_by_available_tools(skills, available_tools);
    if visible.is_empty() {
        return "No skills found in workspace/skills/".to_string();
    }
    let mut out = format!("Skills ({}):\n", visible.len());
    for s in &visible {
        out.push_str(&format!(
            "- **{}** (priority: {}): {}\n  Triggers: {}\n  Tools: {}\n",
            s.meta.name,
            s.meta.priority,
            s.meta.description,
            s.meta.triggers.join(", "),
            if s.meta.tools_required.is_empty() { "all".to_string() } else { s.meta.tools_required.join(", ") },
        ));
    }
    out
}

// ── OpenAPI discovery ──────────────────────────────────────────

/// Tool meta: discover and create draft tools from an OpenAPI/Swagger spec URL.
pub async fn handle_tool_discover(
    workspace_dir: &str,
    ssrf_client: &reqwest::Client,
    args: &serde_json::Value,
) -> String {
    use crate::agent::openapi::{discover_base_url, extract_openapi_tools};

    let spec_url = match args.get("spec_url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => return "Error: 'spec_url' is required".to_string(),
    };
    let prefix = args.get("prefix").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Use SSRF-safe client to prevent LLM-directed requests to internal services
    let spec_text = match ssrf_client
        .get(&spec_url)
        .header("Accept", "application/json, application/yaml, */*")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => match r.text().await {
            Ok(t) => t,
            Err(e) => return format!("Error reading spec: {}", e),
        },
        Err(e) => return format!("Error fetching spec: {}", e),
    };

    let spec: serde_json::Value = if let Ok(v) = serde_json::from_str(&spec_text) {
        v
    } else if let Ok(v) = serde_yaml::from_str::<serde_json::Value>(&spec_text) {
        v
    } else {
        return "Error: could not parse spec as JSON or YAML".to_string();
    };

    let base_url = discover_base_url(&spec, &spec_url);
    let tools = extract_openapi_tools(&spec, &base_url, &prefix);
    if tools.is_empty() {
        return "No API operations found in spec. Make sure it's a valid OpenAPI 2.x/3.x spec.".to_string();
    }

    let draft_dir = std::path::Path::new(workspace_dir)
        .join("tools")
        .join("draft");
    tokio::fs::create_dir_all(&draft_dir).await.ok();

    let mut created = Vec::new();
    let mut errors = Vec::new();

    for tool in &tools {
        let yaml = match serde_yaml::to_string(tool) {
            Ok(y) => y,
            Err(e) => { errors.push(format!("{}: {}", tool.name, e)); continue; }
        };
        let path = draft_dir.join(format!("{}.yaml", tool.name));
        match tokio::fs::write(&path, &yaml).await {
            Ok(_) => created.push(tool.name.clone()),
            Err(e) => errors.push(format!("{}: {}", tool.name, e)),
        }
    }

    let mut out = format!(
        "Discovered {} tools from {}\nCreated {} draft tools:\n",
        tools.len(), spec_url, created.len()
    );
    for name in &created {
        out.push_str(&format!("- {} (draft)\n", name));
    }
    if !errors.is_empty() {
        out.push_str("\nErrors:\n");
        for e in &errors { out.push_str(&format!("- {}\n", e)); }
    }
    out.push_str("\nUse tool_test to verify, then tool_verify to activate.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace_with_skills(skills: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).expect("create skills dir");
        for (name, content) in skills {
            let path = skills_dir.join(format!("{name}.md"));
            std::fs::write(path, content).expect("write skill");
        }
        dir
    }

    #[tokio::test]
    async fn handle_skill_use_list_filters_by_available_tools() {
        let dir = temp_workspace_with_skills(&[
            (
                "needs_code_exec",
                "---\nname: needs_code_exec\ndescription: needs code\ntools_required:\n  - code_exec\n---\n\nbody",
            ),
            (
                "needs_web_fetch",
                "---\nname: needs_web_fetch\ndescription: needs web\ntools_required:\n  - web_fetch\n---\n\nbody",
            ),
            (
                "no_requirements",
                "---\nname: no_requirements\ndescription: free\n---\n\nbody",
            ),
        ]);

        let available: std::collections::HashSet<String> =
            ["web_fetch".to_string()].into_iter().collect();
        let args = serde_json::json!({"action": "list"});

        let result = handle_skill_use(
            dir.path().to_str().unwrap(),
            false, // is_base
            &available,
            &args,
        )
        .await;

        assert!(result.contains("needs_web_fetch"), "web_fetch skill should be visible: {result}");
        assert!(result.contains("no_requirements"), "skills without requirements always visible: {result}");
        assert!(!result.contains("needs_code_exec"), "code_exec skill should be hidden: {result}");
    }

    #[tokio::test]
    async fn handle_skill_use_load_ignores_filter() {
        let dir = temp_workspace_with_skills(&[
            (
                "needs_code_exec",
                "---\nname: needs_code_exec\ndescription: needs code\ntools_required:\n  - code_exec\n---\n\nINSTRUCTIONS",
            ),
        ]);
        let empty: std::collections::HashSet<String> = std::collections::HashSet::new();
        let args = serde_json::json!({"action": "load", "name": "needs_code_exec"});

        let result = handle_skill_use(
            dir.path().to_str().unwrap(),
            false,
            &empty,
            &args,
        )
        .await;

        assert!(result.contains("INSTRUCTIONS"), "load by name must work even when filter would hide: {result}");
    }

    #[tokio::test]
    async fn handle_skill_list_filters_by_available_tools() {
        let dir = temp_workspace_with_skills(&[
            (
                "needs_code_exec",
                "---\nname: needs_code_exec\ndescription: x\ntools_required:\n  - code_exec\n---\n\nbody",
            ),
            (
                "no_requirements",
                "---\nname: no_requirements\ndescription: free\n---\n\nbody",
            ),
        ]);

        let empty: std::collections::HashSet<String> = std::collections::HashSet::new();
        let result = handle_skill_list(
            dir.path().to_str().unwrap(),
            false,
            &empty,
            &serde_json::json!({}),
        )
        .await;

        assert!(result.contains("no_requirements"), "should keep no-requirements skill: {result}");
        assert!(!result.contains("needs_code_exec"), "should hide code_exec skill when empty available set: {result}");
    }
}
