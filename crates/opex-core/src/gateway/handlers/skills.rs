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
        .route("/api/skills/repairs", get(api_skill_repairs_list))
        .route("/api/skills/repairs/{id}", axum::routing::patch(api_skill_repair_resolve))
        .route("/api/skills/{skill}", get(api_skill_get_global).put(api_skill_upsert_global).delete(api_skill_delete_global))
        .route("/api/skills/{skill}/versions", get(api_skill_versions))
        .route("/api/skills/{skill}/versions/{vid}/restore", axum::routing::post(api_skill_version_restore))
        .route("/api/skills/{skill}/pin", axum::routing::patch(api_skill_pin_global))
}

/// Sanitize a skill name to a safe filename stem (same logic as `write_skill`).
///
/// Audit 2026-05-08 path-traversal hardening:
/// - The previous version stripped `/`, `\`, etc. but kept `.`, so a name like
///   `"../agents/Main/SOUL"` survived sanitisation as
///   `"..-agents-Main-SOUL"`… *or rather*, `/` was rewritten so that case was
///   actually safe; but `../skills/x` (with the slash already replaced) was
///   not the only attack: the runtime later joins `skills_dir + sanitised`
///   and any leading `.` made the join nondescript. We now also collapse
///   `.` to `-` so the resulting filename can never start with `..` or `.`,
///   and refuse empty / all-separator inputs at the filesystem layer.
/// - Callers that go on to write/read the file must additionally `canonicalize`
///   the joined path and confirm it stays inside `skills_dir` — see
///   `assert_inside_skills_dir`.
pub(crate) fn skill_safe_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | ' ' | '.' => '-',
            other => other,
        })
        .collect();
    let trimmed = replaced.trim_matches('-').to_string();
    if trimmed.is_empty() {
        // Stable sentinel — `find_skill_path` will fall through to the
        // frontmatter scan or return None; `write_skill` should reject this
        // upstream via `assert_inside_skills_dir`.
        return "_unnamed".to_string();
    }
    trimmed
}

// `skill_safe_name` is the only sanitiser the gateway layer needs — the
// filesystem-touching writes happen inside `crate::skills::write_skill`,
// which performs its own canonical-path check (audit 2026-05-08).

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

    // Use the comprehensive system-tool catalogue (includes `tool_*` family,
    // `memory`, and other tools that `dispatch::SYSTEM_TOOL_NAMES` omits because
    // `filter_tools_by_policy` handles them via dedicated branches). At the API
    // layer we have no `memory_available` flag — assume memory is on (worst case
    // a memory-needing skill is shown but won't actually run; non-blocking).
    let mut all: Vec<String> = crate::agent::pipeline::tool_defs::all_system_tool_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
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

// ── Skill JSON serialization ─────────────────────────────────────────────────

fn skill_to_json(s: &crate::skills::SkillDef) -> serde_json::Value {
    serde_json::json!({
        "name": s.meta.name,
        "description": s.meta.description,
        "triggers": s.meta.triggers,
        "tools_required": s.meta.tools_required,
        "priority": s.meta.priority,
        "instructions_len": s.instructions.len(),
        "state": s.meta.state,
        "last_used_at": s.meta.last_used_at,
        "pinned": s.meta.pinned.unwrap_or(false),
    })
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
    let result: Vec<serde_json::Value> = skills.iter().map(skill_to_json).collect();
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
    // Read existing skill to use as fallback for fields not provided in the request.
    // This ensures state-only updates (e.g. archive/unarchive) preserve all other content.
    let existing_skill = find_skill_path(crate::config::WORKSPACE_DIR, &skill_name).await
        .and_then(|p| std::fs::read_to_string(&p).ok())
        .and_then(|c| crate::skills::SkillDef::parse(&c));

    let existing_meta = existing_skill.as_ref().map(|s| &s.meta);

    let instructions = if body.instructions.is_empty() {
        existing_skill.as_ref().map(|s| s.instructions.as_str()).unwrap_or("").to_string()
    } else {
        body.instructions
    };

    let frontmatter = crate::skills::SkillFrontmatter {
        name: skill_name.clone(),
        description: body.description
            .unwrap_or_else(|| existing_meta.map(|m| m.description.clone()).unwrap_or_default()),
        triggers: if body.triggers.is_empty() {
            existing_meta.map(|m| m.triggers.clone()).unwrap_or_default()
        } else {
            body.triggers
        },
        tools_required: if body.tools_required.is_empty() {
            existing_meta.map(|m| m.tools_required.clone()).unwrap_or_default()
        } else {
            body.tools_required
        },
        priority: if body.priority == 0 {
            existing_meta.map(|m| m.priority).unwrap_or(0)
        } else {
            body.priority
        },
        last_used_at: existing_meta.and_then(|m| m.last_used_at.clone()),
        state: body.state.unwrap_or(crate::skills::SkillState::Active),
        pinned: existing_meta.and_then(|m| m.pinned),
    };
    match crate::skills::write_skill(
        crate::config::WORKSPACE_DIR,
        &skill_name,
        &frontmatter,
        &instructions,
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
    pub(crate) state: Option<crate::skills::SkillState>,
}

#[derive(serde::Deserialize)]
pub(crate) struct SkillPinBody {
    pinned: bool,
}

// ── Skill version endpoints ───────────────────────────────────────────────────

/// GET /api/skills/{skill}/versions
pub(crate) async fn api_skill_versions(
    State(infra): State<InfraServices>,
    axum::extract::Path(skill_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match crate::db::skill_versions::list_versions(&infra.db, &skill_name).await {
        Ok(versions) => Json(serde_json::json!({"versions": versions})).into_response(),
        Err(e) => {
            tracing::error!(skill = %skill_name, error = %e, "failed to list skill versions");
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// GET /api/skills/{skill}/versions/{vid}
pub(crate) async fn api_skill_version_restore(
    State(infra): State<InfraServices>,
    axum::extract::Path((skill_name, vid)): axum::extract::Path<(String, uuid::Uuid)>,
) -> impl IntoResponse {
    let version = match crate::db::skill_versions::get_version(&infra.db, vid).await {
        Ok(Some(v)) => v,
        Ok(None) => return (StatusCode::NOT_FOUND,
                            Json(serde_json::json!({"error": "version not found"}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR,
                          Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    };

    let safe = crate::curator::sanitize_skill_name(&skill_name);
    let path = std::path::Path::new(crate::config::WORKSPACE_DIR)
        .join("skills")
        .join(format!("{safe}.md"));

    // Snapshot current content before overwriting
    if let Ok(current) = tokio::fs::read_to_string(&path).await {
        let _ = crate::db::skill_versions::save_version(
            &infra.db, &skill_name, &current, "restore", None,
            Some("pre-restore snapshot"),
        ).await;
    }

    // Restore: ensure state is active
    let content = version.content
        .replacen("state: archived", "state: active", 1)
        .replacen("state: stale", "state: active", 1);

    match tokio::fs::write(&path, &content).await {
        Ok(()) => {
            tracing::info!(skill = %skill_name, version = %vid, "skill version restored");
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => {
            tracing::error!(skill = %skill_name, error = %e, "failed to restore skill version");
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": e.to_string()}))).into_response()
        }
    }
}


/// PATCH /api/skills/{skill}/pin — set or clear the pinned flag.
///
/// Pinned skills are skipped by the Curator (Phase 1 transitions and
/// Phase 3 consolidation). Uses the same file-write path as upsert so
/// all other frontmatter fields are preserved.
pub(crate) async fn api_skill_pin_global(
    State(_state): State<InfraServices>,
    axum::extract::Path(skill_name): axum::extract::Path<String>,
    axum::extract::Json(body): axum::extract::Json<SkillPinBody>,
) -> impl IntoResponse {
    let Some(path) = find_skill_path(crate::config::WORKSPACE_DIR, &skill_name).await else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "skill not found"}))).into_response();
    };

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    };

    let Some(skill_def) = crate::skills::SkillDef::parse(&content) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "failed to parse skill"}))).into_response();
    };

    let frontmatter = crate::skills::SkillFrontmatter {
        name:           skill_def.meta.name.clone(),
        description:    skill_def.meta.description.clone(),
        triggers:       skill_def.meta.triggers.clone(),
        tools_required: skill_def.meta.tools_required.clone(),
        priority:       skill_def.meta.priority,
        last_used_at:   skill_def.meta.last_used_at.clone(),
        state:          skill_def.meta.state.clone(),
        pinned:         Some(body.pinned),
    };

    match crate::skills::write_skill(
        crate::config::WORKSPACE_DIR,
        &skill_name,
        &frontmatter,
        &skill_def.instructions,
    ).await {
        Ok(()) => {
            tracing::info!(skill = %skill_name, pinned = body.pinned, "skill pin toggled");
            Json(serde_json::json!({"name": skill_name, "pinned": body.pinned})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Skill repair endpoints ────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub(crate) struct RepairsQuery {
    status: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct ResolveRepairBody {
    status: String,
    resolution_note: Option<String>,
}

/// GET /api/skills/repairs[?status=pending]
pub(crate) async fn api_skill_repairs_list(
    State(infra): State<InfraServices>,
    axum::extract::Query(q): axum::extract::Query<RepairsQuery>,
) -> impl IntoResponse {
    match crate::db::skill_repairs::list(
        &infra.db,
        q.status.as_deref(),
        100,
    ).await {
        Ok(rows) => Json(serde_json::json!({"repairs": rows})).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to list skill repairs");
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": "db error"}))).into_response()
        }
    }
}

/// PATCH /api/skills/repairs/{id}
pub(crate) async fn api_skill_repair_resolve(
    State(infra): State<InfraServices>,
    axum::extract::Path(id_str): axum::extract::Path<String>,
    axum::Json(body): axum::Json<ResolveRepairBody>,
) -> impl IntoResponse {
    let id = match uuid::Uuid::parse_str(&id_str) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST,
                          Json(serde_json::json!({"error": "invalid uuid"}))).into_response(),
    };
    if !matches!(body.status.as_str(), "done" | "failed") {
        return (StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "status must be 'done' or 'failed'"}))).into_response();
    }
    match crate::db::skill_repairs::resolve(
        &infra.db, id, &body.status, body.resolution_note.as_deref(),
    ).await {
        Ok(true)  => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND,
                      Json(serde_json::json!({"error": "not found or already resolved"}))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to resolve skill repair");
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(serde_json::json!({"error": "db error"}))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_response_includes_state_and_last_used_at() {
        use crate::skills::{SkillDef, SkillFrontmatter, SkillState};
        let skill = SkillDef {
            meta: SkillFrontmatter {
                name: "test".into(),
                description: "desc".into(),
                triggers: vec![],
                tools_required: vec![],
                priority: 0,
                last_used_at: Some("2026-04-30T12:00:00Z".into()),
                state: SkillState::Stale,
                pinned: None,
            },
            instructions: "body".into(),
        };
        let json = skill_to_json(&skill);
        assert_eq!(json["state"], "stale");
        assert_eq!(json["last_used_at"], "2026-04-30T12:00:00Z");
    }

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
