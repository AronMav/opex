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
use serde::Deserialize;
use std::cmp::Reverse;
use std::path::Path;
use tokio::fs;

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

    let safe_name = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
    let path = skills_dir.join(format!("{safe_name}.md"));

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

    let content = format!(
        "---\nname: {}\ndescription: {}\ntriggers:\n{}\ntools_required:\n{}\npriority: {}\n---\n\n{}",
        frontmatter.name,
        frontmatter.description,
        triggers_yaml,
        tools_yaml,
        frontmatter.priority,
        instructions,
    );

    fs::write(&path, &content).await?;
    tracing::info!(skill = %name, file = %path.display(), "skill written");
    Ok(())
}

/// List skill file names in the shared skills directory.
#[allow(dead_code)]
pub async fn list_skills(workspace_dir: &str) -> Vec<String> {
    let skills_dir = Path::new(workspace_dir).join("skills");
    let mut names = Vec::new();

    let mut read_dir = match fs::read_dir(&skills_dir).await {
        Ok(d) => d,
        Err(_) => return names,
    };

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
    }

    names.sort();
    names
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

    /// Audit: every `tools_required` entry across workspace/ and config/
    /// skills must reference a known tool (core system, YAML, or mcp__-prefixed).
    /// Catches typos that would silently hide a skill from listings.
    #[tokio::test]
    async fn audit_all_skills_required_tools_exist() {
        // Resolve workspace/ relative to the Cargo workspace root (two levels up from
        // crates/hydeclaw-core/), so the test finds real skill files regardless of
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
