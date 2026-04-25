use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use super::super::AppState;
use crate::gateway::clusters::InfraServices;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/skills", get(api_skills_list_global))
        .route("/api/skills/{skill}", get(api_skill_get_global).put(api_skill_upsert_global).delete(api_skill_delete_global))
        .route("/api/agents/{name}/skills", get(api_skills_list))
        .route("/api/agents/{name}/skills/{skill}", get(api_skill_get).put(api_skill_upsert).delete(api_skill_delete))
}

/// Sanitize a skill name to a safe filename stem (same logic as `write_skill`).
pub(crate) fn skill_safe_name(name: &str) -> String {
    name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-")
}

/// Resolve the actual .md file path for a skill name.
/// Tries sanitized-name first, then scans all .md files for matching frontmatter name.
/// Returns None if no matching file found.
pub(crate) async fn find_skill_path(
    workspace_dir: &str,
    skill_name: &str,
) -> Option<std::path::PathBuf> {
    let skills_dir = std::path::PathBuf::from(workspace_dir).join("skills");

    // 1. Try sanitized name (skills created/saved via UI)
    let safe = skill_safe_name(skill_name);
    let candidate = skills_dir.join(format!("{safe}.md"));
    if candidate.exists() {
        return Some(candidate);
    }

    // 2. Fallback: scan all .md files for matching frontmatter name
    let mut rd = tokio::fs::read_dir(&skills_dir).await.ok()?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Ok(content) = tokio::fs::read_to_string(&path).await
            && let Some(skill) = crate::skills::SkillDef::parse(&content)
                && skill.meta.name == skill_name {
                    return Some(path);
                }
    }
    None
}

// ── Available-tools helper for API filtering ─────────────────────────────────

/// Default location of agent config files. Production callers pass
/// this; tests pass a tempdir to avoid cwd dependency.
pub(crate) const DEFAULT_AGENTS_DIR: &str = "config/agents";

/// Build the set of tool names available to the named agent based on
/// on-disk config and YAML tool files. Returns `None` if the agent is
/// unknown (caller falls back to no filtering — see api_skills_list_global).
///
/// Note: this is a synchronous filesystem read inside an async fn. Reading
/// ~10 small TOML files takes ~1ms on SSD; acceptable for an HTTP handler
/// (not a hot path). If profile shows it matters, wrap in `spawn_blocking`.
pub(crate) async fn available_tools_for_agent(
    workspace_dir: &str,
    agents_dir: &str,
    agent_name: &str,
) -> Option<std::collections::HashSet<String>> {
    let agents = crate::config::load_agent_configs(agents_dir).ok()?;
    let cfg = agents.into_iter().find(|c| c.agent.name == agent_name)?;

    let mut all: Vec<String> = crate::agent::pipeline::dispatch::SYSTEM_TOOL_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    // memory is a special-case (gated by memory_available at runtime).
    // We assume memory is available in the API filter; worst case the
    // user sees memory-needing skills they can't actually call.
    all.push("memory".to_string());
    for yt in crate::tools::yaml_tools::load_yaml_tools(workspace_dir, false).await {
        all.push(yt.name);
    }

    let policy = cfg.agent.tools.as_ref();
    let kept: std::collections::HashSet<String> = all
        .into_iter()
        .filter(|name| {
            let Some(p) = policy else { return true };
            if p.deny.iter().any(|d| d == name) {
                return false;
            }
            if p.allow_all {
                return true;
            }
            if p.deny_all_others {
                return p.allow.iter().any(|a| a == name);
            }
            if !p.allow.is_empty() {
                return p.allow.iter().any(|a| a == name);
            }
            true
        })
        .collect();

    Some(kept)
}

// ── Global skills endpoints ───────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub(crate) struct SkillsListQuery {
    agent: Option<String>,
}

/// GET /api/skills (?agent=<name> for tool-aware filtering)
pub(crate) async fn api_skills_list_global(
    State(_state): State<InfraServices>,
    axum::extract::Query(q): axum::extract::Query<SkillsListQuery>,
) -> impl IntoResponse {
    let mut skills = crate::skills::load_skills(crate::config::WORKSPACE_DIR).await;
    if let Some(agent_name) = q.agent.as_deref()
        && let Some(available) = available_tools_for_agent(
            crate::config::WORKSPACE_DIR,
            DEFAULT_AGENTS_DIR,
            agent_name,
        ).await
    {
        skills = crate::skills::filter_skills_by_available_tools(skills, &available);
    }
    let result: Vec<serde_json::Value> = skills.iter().map(|s| {
        serde_json::json!({
            "name": s.meta.name,
            "description": s.meta.description,
            "triggers": s.meta.triggers,
            "tools_required": s.meta.tools_required,
            "priority": s.meta.priority,
            "instructions_len": s.instructions.len(),
        })
    }).collect();
    Json(serde_json::json!({"skills": result})).into_response()
}

/// GET /api/skills/{skill}
pub(crate) async fn api_skill_get_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(skill_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(path) = find_skill_path(crate::config::WORKSPACE_DIR, &skill_name).await else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "skill not found"}))).into_response();
    };

    match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            let mut result = serde_json::json!({ "name": skill_name, "content": content });
            if let Some(skill) = crate::skills::SkillDef::parse(&content) {
                result["description"] = serde_json::json!(skill.meta.description);
                result["triggers"] = serde_json::json!(skill.meta.triggers);
                result["tools_required"] = serde_json::json!(skill.meta.tools_required);
                result["priority"] = serde_json::json!(skill.meta.priority);
                result["instructions"] = serde_json::json!(skill.instructions);
            }
            Json(result).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "skill not found"}))).into_response(),
    }
}

/// PUT /api/skills/{skill}
pub(crate) async fn api_skill_upsert_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(skill_name): axum::extract::Path<String>,
    axum::extract::Json(body): axum::extract::Json<SkillUpsertBody>,
) -> impl IntoResponse {
    let frontmatter = crate::skills::SkillFrontmatter {
        name: skill_name.clone(),
        description: body.description.unwrap_or_default(),
        triggers: body.triggers,
        tools_required: body.tools_required,
        priority: body.priority,
    };
    match crate::skills::write_skill(
        crate::config::WORKSPACE_DIR,
        &skill_name,
        &frontmatter,
        &body.instructions,
    ).await {
        Ok(()) => {
            tracing::info!(skill = %skill_name, "skill upserted via UI (global)");
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

/// DELETE /api/skills/{skill}
pub(crate) async fn api_skill_delete_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(skill_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(path) = find_skill_path(crate::config::WORKSPACE_DIR, &skill_name).await else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "skill not found"}))).into_response();
    };

    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            tracing::info!(skill = %skill_name, "skill deleted via UI (global)");
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Per-agent skills endpoints (compat) ──────────────────────────────────────

/// GET /api/agents/{name}/skills
/// Returns list of all skills, filtered by tools available to the named agent.
pub(crate) async fn api_skills_list(
    State(_state): State<InfraServices>,
    axum::extract::Path(agent_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let mut skills = crate::skills::load_skills(crate::config::WORKSPACE_DIR).await;
    if let Some(available) = available_tools_for_agent(
        crate::config::WORKSPACE_DIR,
        DEFAULT_AGENTS_DIR,
        &agent_name,
    ).await {
        skills = crate::skills::filter_skills_by_available_tools(skills, &available);
    }
    let result: Vec<serde_json::Value> = skills.iter().map(|s| {
        serde_json::json!({
            "name": s.meta.name,
            "description": s.meta.description,
            "triggers": s.meta.triggers,
            "tools_required": s.meta.tools_required,
            "priority": s.meta.priority,
            "instructions_len": s.instructions.len(),
        })
    }).collect();
    Json(serde_json::json!({"skills": result})).into_response()
}

/// GET /api/agents/{name}/skills/{skill}
/// Returns the skill content and parsed structured fields.
pub(crate) async fn api_skill_get(
    State(_state): State<InfraServices>,
    axum::extract::Path((_agent_name, skill_name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let Some(path) = find_skill_path(crate::config::WORKSPACE_DIR, &skill_name).await else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "skill not found"}))).into_response();
    };

    match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            let mut result = serde_json::json!({ "name": skill_name, "content": content });
            if let Some(skill) = crate::skills::SkillDef::parse(&content) {
                result["description"] = serde_json::json!(skill.meta.description);
                result["triggers"] = serde_json::json!(skill.meta.triggers);
                result["tools_required"] = serde_json::json!(skill.meta.tools_required);
                result["priority"] = serde_json::json!(skill.meta.priority);
                result["instructions"] = serde_json::json!(skill.instructions);
            }
            Json(result).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "skill not found"}))).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct SkillUpsertBody {
    description: Option<String>,
    #[serde(default)]
    triggers: Vec<String>,
    #[serde(default)]
    tools_required: Vec<String>,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    pub(crate) instructions: String,
}

/// PUT /api/agents/{name}/skills/{skill}
/// Creates or updates a skill file.
pub(crate) async fn api_skill_upsert(
    State(_state): State<InfraServices>,
    axum::extract::Path((agent_name, skill_name)): axum::extract::Path<(String, String)>,
    axum::extract::Json(body): axum::extract::Json<SkillUpsertBody>,
) -> impl IntoResponse {
    let frontmatter = crate::skills::SkillFrontmatter {
        name: skill_name.clone(),
        description: body.description.unwrap_or_default(),
        triggers: body.triggers,
        tools_required: body.tools_required,
        priority: body.priority,
    };
    match crate::skills::write_skill(
        crate::config::WORKSPACE_DIR,
        &skill_name,
        &frontmatter,
        &body.instructions,
    ).await {
        Ok(()) => {
            tracing::info!(agent = %agent_name, skill = %skill_name, "skill upserted via UI");
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

/// DELETE /api/agents/{name}/skills/{skill}
/// Deletes a skill file.
pub(crate) async fn api_skill_delete(
    State(_state): State<InfraServices>,
    axum::extract::Path((_agent_name, skill_name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let Some(path) = find_skill_path(crate::config::WORKSPACE_DIR, &skill_name).await else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "skill not found"}))).into_response();
    };

    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            tracing::info!(skill = %skill_name, "skill deleted via UI");
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_agent_toml(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).expect("create config dir");
        std::fs::write(dir.join(format!("{name}.toml")), body).expect("write toml");
    }

    #[tokio::test]
    async fn available_tools_for_agent_respects_deny_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agents_dir = dir.path().join("agents");
        write_agent_toml(
            &agents_dir,
            "UnitTestAgent",
            r#"
[agent]
name = "UnitTestAgent"
base = false
provider = "anthropic"
model = "claude-3-5-sonnet-20241022"

[agent.tools]
deny = ["code_exec"]
allow_all = true
"#,
        );

        let workspace = dir.path().to_str().unwrap();
        let agents = agents_dir.to_str().unwrap();
        let available = available_tools_for_agent(workspace, agents, "UnitTestAgent")
            .await
            .expect("agent must be found");

        assert!(!available.contains("code_exec"), "code_exec should be denied");
        assert!(available.contains("workspace_read"), "core tools should remain");
    }

    #[tokio::test]
    async fn available_tools_for_agent_unknown_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agents_dir = dir.path().join("agents");
        std::fs::create_dir_all(&agents_dir).expect("create config dir");

        let result = available_tools_for_agent(
            dir.path().to_str().unwrap(),
            agents_dir.to_str().unwrap(),
            "NonExistentAgent",
        ).await;

        assert!(result.is_none(), "unknown agent should return None");
    }
}
