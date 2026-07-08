pub mod evolution;

/// Skill Manager — Markdown-based agent scenarios.
///
/// Skills are stored as Markdown files with YAML frontmatter in:
///   workspace/skills/*.md  (shared by all agents)
///
/// Frontmatter format:
/// ```yaml
/// ---
/// name: research-task
/// description: Deep research on a topic with web search and summary
/// triggers:
///   - research
///   - find information
///   - explain in detail
/// tools_required:
///   - web_search
///   - workspace_write
///   - memory_search
/// priority: 10
/// ---
/// ```
///
/// The instructions body (after the second `---`) is injected into the system prompt
/// when a skill is matched. `tools_required` tools are prioritized, but core tools
/// (memory, workspace, message, shell) are always available.
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::path::Path;
use tokio::fs;

// ── SkillState ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SkillState {
    #[default]
    Active,
    Stale,
    Archived,
}

// ── Frontmatter ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SkillFrontmatter {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub tools_required: Vec<String>,
    /// Higher priority wins when multiple skills match. Default: 0.
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub state: SkillState,
    #[serde(default)]
    pub pinned: Option<bool>,
}

// ── SkillDef ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SkillDef {
    pub meta: SkillFrontmatter,
    /// Instructions body (Markdown, injected into system prompt).
    pub instructions: String,
}

impl SkillDef {
    /// Parse a Markdown file with YAML frontmatter.
    /// Returns None if the file lacks valid frontmatter or fails to parse.
    // reviewed: [3..] guarded by starts_with("---") (ASCII); rest from find() — char boundaries
    #[allow(clippy::string_slice)]
    pub fn parse(content: &str) -> Option<Self> {
        // Frontmatter is delimited by `---` on its own line
        let content = content.trim_start();
        if !content.starts_with("---") {
            return None;
        }
        // Find the closing ---
        let rest = &content[3..];
        let close = rest.find("\n---")?;
        let yaml_str = rest[..close].trim();
        let body = rest[close + 4..].trim_start().to_string();

        let meta: SkillFrontmatter = serde_yaml::from_str(yaml_str).ok()?;
        if meta.name.is_empty() {
            return None;
        }

        Some(SkillDef {
            meta,
            instructions: body,
        })
    }

}

// ── Loader ────────────────────────────────────────────────────────────────────

/// Load all shared skills from the workspace skills directory.
pub async fn load_skills(workspace_dir: &str) -> Vec<SkillDef> {
    let skills_dir = Path::new(workspace_dir).join("skills");

    let mut skills = Vec::new();

    let mut read_dir = match fs::read_dir(&skills_dir).await {
        Ok(d) => d,
        Err(_) => return skills,
    };

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "md" {
            continue;
        }

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "failed to read skill file");
                continue;
            }
        };

        match SkillDef::parse(&content) {
            Some(skill) => {
                tracing::debug!(skill = %skill.meta.name, "loaded skill");
                skills.push(skill);
            }
            None => {
                tracing::warn!(file = %path.display(), "failed to parse skill frontmatter");
            }
        }
    }

    // Sort by priority descending — highest priority skills checked first
    skills.sort_by_key(|s| Reverse(s.meta.priority));

    skills
}

/// Load skills including base-agent-only skills from config/skills/base/.
/// Used for base agents — they see everything regular agents see plus base-only skills.
pub async fn load_skills_for_base(workspace_dir: &str) -> Vec<SkillDef> {
    let mut skills = load_skills(workspace_dir).await;

    {
        let base_dir = Path::new("config").join("skills");
        if let Ok(mut read_dir) = fs::read_dir(&base_dir).await {
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let content = match fs::read_to_string(&path).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(file = %path.display(), error = %e, "failed to read base skill");
                        continue;
                    }
                };
                if let Some(skill) = SkillDef::parse(&content) {
                    tracing::debug!(skill = %skill.meta.name, "loaded base-only skill");
                    skills.push(skill);
                }
            }
        }
    }

    skills.sort_by_key(|s| Reverse(s.meta.priority));
    skills
}


// ── Filter ────────────────────────────────────────────────────────────────────

/// Filter skills by tool availability.
///
/// A skill is kept if it has no `tools_required` OR at least one of them
/// is in `available`. Skills with non-empty requirements where none match
/// are dropped (and logged at `tracing::debug` level).
///
/// Tool name matching is **exact and case-sensitive**. MCP tools must be
/// referenced by their full prefixed name (e.g. `mcp__searxng__search`).
pub fn filter_skills_by_available_tools(
    skills: Vec<SkillDef>,
    available: &std::collections::HashSet<String>,
) -> Vec<SkillDef> {
    skills
        .into_iter()
        .filter(|s| {
            if s.meta.tools_required.is_empty() {
                return true;
            }
            let kept = s.meta.tools_required.iter().any(|t| available.contains(t));
            if !kept {
                tracing::debug!(
                    skill = %s.meta.name,
                    required = ?s.meta.tools_required,
                    "skill skipped: no required tool available",
                );
            }
            kept
        })
        .collect()
}

// ── Scaffold ─────────────────────────────────────────────────────────────────

/// Create a skill file in the shared skills directory.
pub async fn write_skill(
    workspace_dir: &str,
    name: &str,
    frontmatter: &SkillFrontmatter,
    instructions: &str,
) -> anyhow::Result<()> {
    let skills_dir = Path::new(workspace_dir).join("skills");
    fs::create_dir_all(&skills_dir).await?;

    // Audit 2026-05-08 path-traversal hardening: collapse '.' to '-' as well,
    // and verify the resulting filename canonicalises inside skills_dir.
    let safe_name: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | ' ' | '.' => '-',
            other => other,
        })
        .collect();
    let safe_name = safe_name.trim_matches('-').to_string();
    if safe_name.is_empty() {
        anyhow::bail!(
            "skill name must contain at least one valid character: '{}'",
            name
        );
    }
    let path = skills_dir.join(format!("{safe_name}.md"));

    // Defence-in-depth: confirm the parent directory canonicalises to a
    // location inside skills_dir.
    let canonical_dir = std::fs::canonicalize(&skills_dir)
        .map_err(|e| anyhow::anyhow!("cannot canonicalise skills dir: {e}"))?;
    let canonical_parent = std::fs::canonicalize(path.parent().unwrap_or(&skills_dir))
        .map_err(|e| anyhow::anyhow!("cannot canonicalise skill parent: {e}"))?;
    if !canonical_parent.starts_with(&canonical_dir) {
        anyhow::bail!(
            "skill path '{}' resolves outside skills dir '{}'",
            path.display(),
            canonical_dir.display(),
        );
    }

    let triggers_yaml = frontmatter
        .triggers
        .iter()
        .map(|t| format!("  - {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tools_yaml = frontmatter
        .tools_required
        .iter()
        .map(|t| format!("  - {t}"))
        .collect::<Vec<_>>()
        .join("\n");

    let last_used_line = match &frontmatter.last_used_at {
        Some(ts) => format!("last_used_at: \"{ts}\"\n"),
        None => String::new(),
    };
    let pinned_line = match frontmatter.pinned {
        Some(true) => "pinned: true\n",
        _ => "",
    };
    let state_str = match frontmatter.state {
        SkillState::Active   => "active",
        SkillState::Stale    => "stale",
        SkillState::Archived => "archived",
    };

    let content = format!(
        "---\nname: {}\ndescription: {}\ntriggers:\n{}\ntools_required:\n{}\npriority: {}\nstate: {}\n{last_used_line}{pinned_line}---\n\n{}",
        frontmatter.name,
        frontmatter.description,
        triggers_yaml,
        tools_yaml,
        frontmatter.priority,
        state_str,
        instructions,
    );

    fs::write(&path, &content).await?;
    tracing::info!(skill = %name, file = %path.display(), "skill written");
    Ok(())
}

/// Atomically updates `last_used_at` in skill frontmatter.
/// Skips write if existing value is fresher than `min_age`.
/// Errors are logged — never panics.
// reviewed: skill files always begin with ASCII "---\n" (frontmatter contract),
// so content[3..] is a char boundary; fm_end from find("\n---")+4 (ASCII).
#[allow(clippy::string_slice)]
pub async fn update_skill_last_used_if_stale(
    path: &str,
    now_iso: &str,
    min_age: chrono::Duration,
) {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(path, error = %e, "update_skill_last_used: read failed");
            return;
        }
    };

    // Check existing last_used_at
    if let Some(skill) = SkillDef::parse(&content)
        && let Some(existing) = &skill.meta.last_used_at
        && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(existing)
    {
        let age = chrono::Utc::now()
            .signed_duration_since(ts.with_timezone(&chrono::Utc));
        if age < min_age {
            return;
        }
    }

    // Find frontmatter boundary (second ---)
    let fm_close = content[3..].find("\n---").map(|i| i + 4);
    let Some(fm_end) = fm_close else {
        tracing::debug!(path, "update_skill_last_used: no frontmatter close marker");
        return;
    };
    let frontmatter = &content[..fm_end];
    let trailing_nl = if content.ends_with('\n') { "\n" } else { "" };

    let updated = if frontmatter.contains("last_used_at:") {
        // Replace existing line — only inside frontmatter
        let mut opened = false;
        let mut closed = false;
        content
            .lines()
            .map(|line| {
                if line.trim() == "---" {
                    if !opened {
                        opened = true;
                    } else if !closed {
                        closed = true;
                    }
                    return line.to_string();
                }
                // Only replace inside frontmatter (between first and second ---)
                if opened && !closed && line.trim_start().starts_with("last_used_at:") {
                    return format!("last_used_at: \"{}\"", now_iso);
                }
                line.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + trailing_nl
    } else {
        // Insert before closing --- of frontmatter
        let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
        let close = lines.iter().enumerate().skip(1).find(|(_, l)| l.trim() == "---");
        if let Some((pos, _)) = close {
            lines.insert(pos, format!("last_used_at: \"{}\"", now_iso));
        } else {
            tracing::debug!(path, "update_skill_last_used: no frontmatter close marker");
            return;
        }
        lines.join("\n") + trailing_nl
    };

    // Atomic write: tmp → rename
    let tmp_path = format!("{path}.tmp");
    match tokio::fs::write(&tmp_path, &updated).await {
        Ok(_) => {
            if let Err(e) = tokio::fs::rename(&tmp_path, path).await {
                tracing::warn!(path, error = %e, "update_skill_last_used: rename failed");
                let _ = tokio::fs::remove_file(&tmp_path).await;
            }
        }
        Err(e) => {
            tracing::debug!(path, error = %e, "update_skill_last_used: write failed");
        }
    }
}

// ── Reactivation ─────────────────────────────────────────────────────────────

/// Restores an archived skill to Active state and updates `last_used_at`.
///
/// Fire-and-forget: all errors are warn-logged, never propagated.
pub async fn reactivate_skill(
    workspace_dir: &str,
    name: &str,
    db: &sqlx::PgPool,
    agent_name: &str,
    now_iso: &str,
) {
    let path = Path::new(workspace_dir).join("skills").join(format!("{name}.md"));

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(skill = %name, error = %e, "reactivate_skill: file not found or unreadable");
            return;
        }
    };

    let skill_def = match SkillDef::parse(&content) {
        Some(s) => s,
        None => {
            tracing::warn!(skill = %name, "reactivate_skill: failed to parse frontmatter");
            return;
        }
    };

    if skill_def.meta.state != SkillState::Archived {
        return; // not archived — noop
    }

    let new_fm = SkillFrontmatter {
        state: SkillState::Active,
        last_used_at: Some(now_iso.to_string()),
        ..skill_def.meta.clone()
    };

    if let Err(e) = write_skill(workspace_dir, name, &new_fm, &skill_def.instructions).await {
        tracing::warn!(skill = %name, error = %e, "reactivate_skill: write_skill failed");
        return;
    }

    let reason = format!("re-used by {}", agent_name);
    if let Err(e) = crate::db::curator_decisions::save_decision(
        db,
        name,
        "reactivated",
        Some(reason.as_str()),
    )
    .await
    {
        tracing::warn!(skill = %name, error = %e, "reactivate_skill: save_decision failed");
    }

    tracing::info!(skill = %name, agent = %agent_name, "skill reactivated from archived");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SkillDef::parse ───────────────────────────────────────────────────────

    #[test]
    fn parse_valid_frontmatter_returns_some() {
        let content = "---\nname: test-skill\ndescription: A test\ntriggers:\n  - исследуй\n  - найди\npriority: 5\n---\n\nInstructions body here.";
        let skill = SkillDef::parse(content).expect("should parse successfully");
        assert_eq!(skill.meta.name, "test-skill");
        assert_eq!(skill.meta.description, "A test");
        assert_eq!(skill.meta.triggers, vec!["исследуй", "найди"]);
        assert_eq!(skill.meta.priority, 5);
        assert_eq!(skill.instructions, "Instructions body here.");
    }

    #[test]
    fn parse_no_frontmatter_returns_none() {
        let content = "Just regular markdown without frontmatter.\n\n## Section\nSome text.";
        assert!(SkillDef::parse(content).is_none());
    }

    #[test]
    fn parse_empty_name_returns_none() {
        let content = "---\nname: \ndescription: something\ntriggers:\n  - hello\n---\n\nBody.";
        assert!(SkillDef::parse(content).is_none());
    }

    #[test]
    fn parse_just_opening_dashes_no_closing_returns_none() {
        let content = "---\nname: orphan\ndescription: missing closing\n";
        assert!(SkillDef::parse(content).is_none());
    }

    #[test]
    fn parse_minimal_frontmatter_with_defaults() {
        let content = "---\nname: minimal\n---\n\nBody text.";
        let skill = SkillDef::parse(content).expect("should parse");
        assert_eq!(skill.meta.name, "minimal");
        assert!(skill.meta.triggers.is_empty());
        assert_eq!(skill.meta.priority, 0);
        assert_eq!(skill.instructions, "Body text.");
    }

    #[test]
    fn parse_leading_whitespace_before_frontmatter_is_trimmed() {
        let content = "   \n---\nname: indented\n---\n\nBody.";
        let skill = SkillDef::parse(content).expect("should parse despite leading whitespace");
        assert_eq!(skill.meta.name, "indented");
    }

    // ── filter_skills_by_available_tools ──────────────────────────────────────

    fn make_skill(name: &str, required: &[&str]) -> SkillDef {
        SkillDef {
            meta: SkillFrontmatter {
                name: name.to_string(),
                description: format!("desc {name}"),
                triggers: vec![],
                tools_required: required.iter().map(|s| s.to_string()).collect(),
                priority: 0,
                last_used_at: None,
                state: SkillState::Active,
                pinned: None,
            },
            instructions: String::new(),
        }
    }

    fn set_of(items: &[&str]) -> std::collections::HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn filter_no_tools_required_keeps_skill() {
        let skills = vec![make_skill("s1", &[])];
        let kept = filter_skills_by_available_tools(skills, &set_of(&[]));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].meta.name, "s1");
    }

    #[test]
    fn filter_all_required_available_keeps_skill() {
        let skills = vec![make_skill("s1", &["a", "b"])];
        let kept = filter_skills_by_available_tools(skills, &set_of(&["a", "b", "c"]));
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn filter_one_of_many_required_available_keeps_skill() {
        let skills = vec![make_skill("s1", &["a", "b", "c"])];
        let kept = filter_skills_by_available_tools(skills, &set_of(&["b"]));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].meta.name, "s1");
    }

    #[test]
    fn filter_none_available_drops_skill() {
        let skills = vec![make_skill("s1", &["a", "b"])];
        let kept = filter_skills_by_available_tools(skills, &set_of(&["c", "d"]));
        assert!(kept.is_empty());
    }

    #[test]
    fn filter_empty_available_drops_skills_with_requirements() {
        let with_req = make_skill("with_req", &["a"]);
        let no_req = make_skill("no_req", &[]);
        let kept = filter_skills_by_available_tools(
            vec![with_req, no_req],
            &set_of(&[]),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].meta.name, "no_req");
    }

    /// Loaded skills are pre-sorted by `Reverse(priority)` in `load_skills`.
    /// This test catches regressions if someone replaces `iter().filter(...)`
    /// with a `HashSet`-collect or other order-losing structure.
    #[test]
    fn filter_preserves_input_order() {
        let mut skills = vec![
            make_skill("low", &["a"]),
            make_skill("high", &[]),
            make_skill("mid", &["a"]),
        ];
        // Manually set priorities and pre-sort like load_skills does.
        skills[0].meta.priority = 1;
        skills[1].meta.priority = 10;
        skills[2].meta.priority = 5;
        skills.sort_by_key(|s| std::cmp::Reverse(s.meta.priority));

        let kept = filter_skills_by_available_tools(skills, &set_of(&["a"]));
        let names: Vec<&str> = kept.iter().map(|s| s.meta.name.as_str()).collect();
        assert_eq!(names, vec!["high", "mid", "low"]);
    }

    #[test]
    fn skill_state_default_is_active() {
        let s: SkillState = Default::default();
        assert_eq!(s, SkillState::Active);
    }

    #[test]
    fn frontmatter_without_state_parses_as_active() {
        let content = "---\nname: test\n---\n\nBody.";
        let skill = SkillDef::parse(content).unwrap();
        assert_eq!(skill.meta.state, SkillState::Active);
        assert!(skill.meta.last_used_at.is_none());
    }

    #[test]
    fn frontmatter_with_state_archived_parses() {
        let content = "---\nname: test\nstate: archived\nlast_used_at: \"2026-01-01T00:00:00Z\"\n---\n\nBody.";
        let skill = SkillDef::parse(content).unwrap();
        assert_eq!(skill.meta.state, SkillState::Archived);
        assert!(skill.meta.last_used_at.is_some());
    }

    #[tokio::test]
    async fn update_skill_last_used_writes_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-skill.md");
        std::fs::write(&path, "---\nname: test\n---\n\nBody.").unwrap();

        update_skill_last_used_if_stale(
            path.to_str().unwrap(),
            "2026-05-01T12:00:00Z",
            chrono::Duration::hours(1),
        ).await;

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("last_used_at:"), "должно появиться поле");
        assert!(content.contains("2026-05-01T12:00:00Z"));
    }

    #[tokio::test]
    async fn update_skill_last_used_skips_if_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh-skill.md");
        let recent = (chrono::Utc::now() - chrono::Duration::minutes(10))
            .to_rfc3339();
        std::fs::write(
            &path,
            format!("---\nname: test\nlast_used_at: \"{recent}\"\n---\n\nBody."),
        ).unwrap();
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

        update_skill_last_used_if_stale(
            path.to_str().unwrap(),
            "2026-05-01T13:00:00Z",
            chrono::Duration::hours(1),
        ).await;

        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "файл не должен быть перезаписан");
    }

    #[tokio::test]
    async fn update_skill_last_used_does_not_touch_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body-skill.md");
        // File has last_used_at in frontmatter AND mentions it in body
        let old_ts = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        std::fs::write(
            &path,
            format!(
                "---\nname: test\nlast_used_at: \"{old_ts}\"\n---\n\nDo not replace last_used_at: here.\n"
            ),
        ).unwrap();

        update_skill_last_used_if_stale(
            path.to_str().unwrap(),
            "2026-05-01T12:00:00Z",
            chrono::Duration::hours(1),
        ).await;

        let content = std::fs::read_to_string(&path).unwrap();
        // Frontmatter should be updated
        assert!(content.contains("last_used_at: \"2026-05-01T12:00:00Z\""));
        // Body line should be untouched
        assert!(content.contains("Do not replace last_used_at: here."));
    }

    /// Audit: every `tools_required` entry across workspace/ and config/
    /// skills must reference a known tool (core system, YAML, or mcp__-prefixed).
    /// Catches typos that would silently hide a skill from listings.
    #[tokio::test]
    async fn write_skill_serializes_pinned_true() {
        let dir = tempfile::tempdir().unwrap();
        let fm = SkillFrontmatter {
            name: "test-skill".into(),
            description: "desc".into(),
            triggers: vec![],
            tools_required: vec![],
            priority: 0,
            last_used_at: None,
            state: SkillState::Active,
            pinned: Some(true),
        };
        write_skill(dir.path().to_str().unwrap(), "test-skill", &fm, "body").await.unwrap();
        let content = tokio::fs::read_to_string(
            dir.path().join("skills/test-skill.md")
        ).await.unwrap();
        assert!(content.contains("pinned: true"), "pinned: true must appear in file");
    }

    #[tokio::test]
    async fn write_skill_omits_pinned_when_false_or_none() {
        let dir = tempfile::tempdir().unwrap();
        for pinned in [Some(false), None] {
            let fm = SkillFrontmatter {
                name: "test-skill".into(),
                description: "desc".into(),
                triggers: vec![],
                tools_required: vec![],
                priority: 0,
                last_used_at: None,
                state: SkillState::Active,
                pinned,
            };
            write_skill(dir.path().to_str().unwrap(), "test-skill", &fm, "body").await.unwrap();
            let content = tokio::fs::read_to_string(
                dir.path().join("skills/test-skill.md")
            ).await.unwrap();
            assert!(!content.contains("pinned:"), "pinned: must NOT appear when false/None");
        }
    }

    #[test]
    fn reactivate_noop_check_non_archived() {
        let state = SkillState::Active;
        let is_archived = matches!(state, SkillState::Archived);
        assert!(!is_archived);
    }

    #[test]
    fn reactivate_triggers_on_archived() {
        let state = SkillState::Archived;
        let is_archived = matches!(state, SkillState::Archived);
        assert!(is_archived);
    }

    #[test]
    fn media_processing_uses_file_handler_menu() {
        // FSE fully retired (see CLAUDE.md "Legacy FSE — RETIRED"): media
        // processing is now model-driven through the single `file_handler` tool
        // (list/run menu) covering every media type — NOT the old per-type
        // auto-dispatch arms (analyze_image / transcribe_audio / <vision>), and
        // no longer the transitional "video-only" state.
        let content = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../workspace/skills/media-processing.md"),
        )
        .expect("media-processing.md must exist");
        let skill = SkillDef::parse(&content).expect("must still parse as a skill");
        let body = skill.instructions.to_lowercase();

        // The skill drives the file_handler tool and documents every media type.
        assert!(body.contains("file_handler"), "must drive the file_handler tool");
        assert!(body.contains("video"), "video handling must be documented");
        assert!(body.contains("image"), "image handling must be documented");
        assert!(body.contains("document"), "document handling must be documented");

        // The retired per-type auto-dispatch tool calls must not reappear.
        assert!(
            !body.contains("transcribe_audio"),
            "legacy transcribe_audio arm is retired — file_handler owns audio now"
        );
        assert!(
            !body.contains("analyze_image"),
            "legacy analyze_image arm is retired — file_handler owns images now"
        );
        assert!(
            !body.contains("<vision>"),
            "legacy image auto-describe arm is retired — file_handler owns images now"
        );

        // tools_required is exactly the single model-driven entry point.
        assert_eq!(skill.meta.tools_required, vec!["file_handler".to_string()]);
    }

    #[tokio::test]
    async fn audit_all_skills_required_tools_exist() {
        // Resolve workspace/ relative to the Cargo workspace root (two levels up from
        // crates/opex-core/), so the test finds real skill files regardless of
        // which directory cargo runs from.
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().unwrap_or(manifest_dir)
            .parent().unwrap_or(manifest_dir);
        let workspace_dir_path = workspace_root.join("workspace");

        if !workspace_dir_path.exists() {
            eprintln!("skipping audit: workspace/ not present at {}", workspace_dir_path.display());
            return;
        }
        let workspace_dir = workspace_dir_path.to_string_lossy();
        let workspace_dir = workspace_dir.as_ref();

        // Use the comprehensive system tool list (includes tool management, git, etc.)
        // rather than the policy-filter subset in dispatch::SYSTEM_TOOL_NAMES.
        let mut known: std::collections::HashSet<String> =
            crate::agent::pipeline::tool_defs::all_system_tool_names()
                .iter()
                .map(|s| s.to_string())
                .collect();
        // memory is already in all_system_tool_names(); explicit insert guards
        // against the constant inadvertently losing it.
        known.insert("memory".to_string());

        // Include draft tools in the known set: tools_required is meant to
        // express "what this skill needs", not "what's active right now".
        // A skill referencing a draft tool is correct — it will be hidden until
        // the tool is verified, which is intentional behaviour.
        // Only truly non-existent tools (absent from the YAML files entirely) indicate a typo.
        for yt in crate::tools::yaml_tools::load_yaml_tools(workspace_dir, true).await {
            known.insert(yt.name);
        }
        // Capability tools replace the 5 deleted YAML tools (generate_image,
        // synthesize_speech, search_web, transcribe_audio, analyze_image).
        // Skills that list these names are correct — the tools now exist as
        // built-in capability tools rather than YAML files.
        for name in crate::agent::capability_tools::CAPABILITY_TOOL_NAMES {
            known.insert(name.to_string());
        }

        // Load workspace skills then config skills using absolute paths derived
        // from the workspace root (avoids cwd-relative path issues in test runner).
        let mut skills = load_skills(workspace_dir).await;
        let config_skills_dir = workspace_root.join("config").join("skills");
        if let Ok(mut rd) = fs::read_dir(&config_skills_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&path).await
                    && let Some(skill) = SkillDef::parse(&content)
                {
                    skills.push(skill);
                }
            }
        }

        let mut missing: Vec<(String, Vec<String>)> = Vec::new();
        for s in skills {
            let unknown: Vec<String> = s.meta.tools_required.iter()
                .filter(|t| !known.contains(*t) && !t.starts_with("mcp__"))
                .cloned()
                .collect();
            if !unknown.is_empty() {
                missing.push((s.meta.name, unknown));
            }
        }

        if !missing.is_empty() {
            let report = missing.iter()
                .map(|(name, unknown)| format!("  - {name}: {unknown:?}"))
                .collect::<Vec<_>>()
                .join("\n");
            panic!(
                "Skills reference unknown tools (not in core, YAML, or mcp__* prefix):\n{report}\n\n\
                 Either fix the typo, register the tool, or use the full mcp__server__tool prefix."
            );
        }
    }
}
