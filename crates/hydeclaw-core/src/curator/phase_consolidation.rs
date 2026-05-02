//! Phase 3 — LLM-driven skill consolidation.
//!
//! Analyzes the active skill collection via a single LLM call, receives a JSON
//! array of curator commands (SKIP/ARCHIVE/MERGE/FIX/RENAME), validates each
//! command, then executes the valid ones.  MERGE and FIX issue a second LLM
//! call to generate the actual file content.

use std::sync::Arc;
use serde::Deserialize;
use crate::agent::providers::LlmProvider;

// ── Public result type ────────────────────────────────────────────────────────

pub struct ConsolidationResult {
    pub commands_executed: i32,
    pub log: Vec<String>,
}

// ── Command enum ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CuratorCommand {
    Skip,
    Archive { skill: String, reason: String },
    Merge { sources: Vec<String>, into: String, reason: String },
    Fix { skill: String, patch: String },
    Rename { skill: String, new_name: String, reason: String },
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse commands from LLM JSON output, silently skipping invalid entries.
pub fn parse_commands(json: &str) -> Vec<CuratorCommand> {
    let raw: Vec<serde_json::Value> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "phase3: failed to parse LLM JSON");
            return vec![];
        }
    };

    raw.into_iter()
        .filter_map(|v| match serde_json::from_value::<CuratorCommand>(v.clone()) {
            Ok(cmd) => Some(cmd),
            Err(e) => {
                tracing::warn!(command = %v, error = %e, "phase3: skipping invalid command");
                None
            }
        })
        .collect()
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Validate a command against the current skill collection.
/// Returns `Some(error_message)` if invalid, `None` if valid.
pub fn validate_command(
    cmd: &CuratorCommand,
    skill_names: &[String],
    pinned: &[String],
) -> Option<String> {
    match cmd {
        CuratorCommand::Skip => None,

        CuratorCommand::Archive { skill, .. } => {
            if !skill_names.contains(skill) {
                return Some(format!("skill not found: {skill}"));
            }
            if pinned.contains(skill) {
                return Some(format!("skill is pinned: {skill}"));
            }
            None
        }

        CuratorCommand::Merge { sources, into: _, .. } => {
            if sources.len() < 2 {
                return Some("MERGE requires >= 2 sources".into());
            }
            for s in sources {
                if !skill_names.contains(s) {
                    return Some(format!("source skill not found: {s}"));
                }
                if pinned.contains(s) {
                    return Some(format!("source skill is pinned: {s}"));
                }
            }
            None
        }

        CuratorCommand::Fix { skill, .. } => {
            if !skill_names.contains(skill) {
                return Some(format!("skill not found: {skill}"));
            }
            if pinned.contains(skill) {
                return Some(format!("skill is pinned: {skill}"));
            }
            None
        }

        CuratorCommand::Rename { skill, new_name, .. } => {
            if !skill_names.contains(skill) {
                return Some(format!("skill not found: {skill}"));
            }
            if pinned.contains(skill) {
                return Some(format!("skill is pinned: {skill}"));
            }
            if skill_names.contains(new_name) {
                return Some(format!("target name already exists: {new_name}"));
            }
            None
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    provider: &Arc<dyn LlmProvider>,
) -> anyhow::Result<ConsolidationResult> {
    let skills = crate::skills::load_skills(workspace_dir).await;
    let active: Vec<_> = skills
        .iter()
        .filter(|s| !matches!(s.meta.state, crate::skills::SkillState::Archived))
        .collect();

    if active.is_empty() {
        return Ok(ConsolidationResult {
            commands_executed: 0,
            log: vec!["no active skills".into()],
        });
    }

    let skill_names: Vec<String> = active.iter().map(|s| s.meta.name.clone()).collect();
    let pinned: Vec<String> = active
        .iter()
        .filter(|s| s.meta.pinned.unwrap_or(false))
        .map(|s| s.meta.name.clone())
        .collect();

    let summary = active
        .iter()
        .map(|s| {
            format!(
                "- name: {}\n  description: {}\n  state: {:?}\n  last_used_at: {}\n  triggers: [{}]",
                s.meta.name,
                s.meta.description,
                s.meta.state,
                s.meta.last_used_at.as_deref().unwrap_or("never"),
                s.meta.triggers.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are a skill collection curator. Analyze these skills and return a JSON array of commands.\n\
         Rules:\n\
         - NEVER touch skills with pinned=true (pinned: [{}])\n\
         - NO DELETE — only ARCHIVE\n\
         - Maximum 5 commands\n\
         - MERGE requires >= 2 sources\n\
         - Return ONLY valid JSON, no explanation\n\n\
         Commands: SKIP | ARCHIVE(skill,reason) | MERGE(sources[],into,reason) | FIX(skill,patch) | RENAME(skill,new_name,reason)\n\n\
         Skills:\n{}\n\n\
         Return JSON array. Example: [{{\"op\":\"SKIP\"}}]",
        pinned.join(", "),
        summary
    );

    let json_raw = llm_call(provider, &prompt).await?;
    let json = json_raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let commands = parse_commands(json);
    let mut result = ConsolidationResult {
        commands_executed: 0,
        log: Vec::new(),
    };

    for cmd in commands {
        if let Some(err) = validate_command(&cmd, &skill_names, &pinned) {
            tracing::warn!(error = %err, "phase3: validation failed, skipping");
            continue;
        }
        match execute_command(&cmd, workspace_dir, db, provider).await {
            Ok(msg) => {
                result.commands_executed += 1;
                result.log.push(msg);
            }
            Err(e) => tracing::warn!(error = %e, "phase3: command execution failed"),
        }
    }

    Ok(result)
}

// ── Command execution ─────────────────────────────────────────────────────────

async fn execute_command(
    cmd: &CuratorCommand,
    workspace_dir: &str,
    db: &sqlx::PgPool,
    provider: &Arc<dyn LlmProvider>,
) -> anyhow::Result<String> {
    let skills_dir = std::path::Path::new(workspace_dir).join("skills");

    match cmd {
        CuratorCommand::Skip => Ok("SKIP".into()),

        CuratorCommand::Archive { skill, reason } => {
            let path =
                crate::gateway::find_skill_path(workspace_dir, skill)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("skill not found: {skill}"))?;
            let content = tokio::fs::read_to_string(&path).await?;
            let _ = crate::db::skill_versions::save_version(
                db,
                skill,
                &content,
                "archive",
                None,
                Some("curator:archive"),
            )
            .await;
            let updated = update_state_in_frontmatter(&content, "archived");
            tokio::fs::write(&path, &updated).await?;
            Ok(format!("ARCHIVE {skill}: {reason}"))
        }

        CuratorCommand::Merge { sources, into, reason } => {
            let mut source_bodies: Vec<(String, String)> = Vec::new();
            for src in sources {
                let path =
                    crate::gateway::find_skill_path(workspace_dir, src)
                        .await
                        .ok_or_else(|| anyhow::anyhow!("source skill not found: {src}"))?;
                source_bodies.push((src.clone(), tokio::fs::read_to_string(&path).await?));
            }

            let bodies_text = source_bodies
                .iter()
                .map(|(n, b)| format!("=== {n} ===\n{b}"))
                .collect::<Vec<_>>()
                .join("\n\n");

            let prompt = format!(
                "Merge these skills into a unified skill named '{into}'. \
                 Return a complete skill file with YAML frontmatter and instructions.\n\n\
                 Reason: {reason}\n\n{bodies_text}"
            );
            let merged_content = llm_call(provider, &prompt).await?;

            let safe_into =
                into.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
            tokio::fs::write(
                skills_dir.join(format!("{safe_into}.md")),
                &merged_content,
            )
            .await?;
            let _ = crate::db::skill_versions::save_version(
                db,
                into,
                &merged_content,
                "merge",
                None,
                Some("curator:merge"),
            )
            .await;

            for (src, body) in &source_bodies {
                if let Some(p) =
                    crate::gateway::find_skill_path(workspace_dir, src).await
                {
                    let _ = crate::db::skill_versions::save_version(
                        db,
                        src,
                        body,
                        "archive",
                        None,
                        Some("curator:merge-archive"),
                    )
                    .await;
                    let _ =
                        tokio::fs::write(&p, update_state_in_frontmatter(body, "archived")).await;
                }
            }

            Ok(format!("MERGE {:?} -> {into}: {reason}", sources))
        }

        CuratorCommand::Fix { skill, patch } => {
            let path =
                crate::gateway::find_skill_path(workspace_dir, skill)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("skill not found: {skill}"))?;
            let existing = tokio::fs::read_to_string(&path).await?;

            // Locate end of frontmatter (second `---`).
            let fm_end = existing[3..]
                .find("\n---")
                .map(|i| i + 7)
                .unwrap_or(existing.len());

            let prompt = format!(
                "Apply this patch to the skill body. \
                 Return ONLY the updated skill body (no frontmatter).\n\n\
                 Patch: {patch}\n\nCurrent body:\n{}",
                &existing[fm_end..]
            );
            let new_body = llm_call(provider, &prompt).await?;

            let _ = crate::db::skill_versions::save_version(
                db,
                skill,
                &existing,
                "fix",
                None,
                Some("curator:fix"),
            )
            .await;
            tokio::fs::write(
                &path,
                format!("{}\n{}", &existing[..fm_end], new_body.trim()),
            )
            .await?;
            Ok(format!("FIX {skill}: {patch}"))
        }

        CuratorCommand::Rename { skill, new_name, reason } => {
            let old_path =
                crate::gateway::find_skill_path(workspace_dir, skill)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("skill not found: {skill}"))?;
            let content = tokio::fs::read_to_string(&old_path).await?;
            let updated =
                content.replacen(&format!("name: {skill}"), &format!("name: {new_name}"), 1);

            let safe_new =
                new_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
            let _ = crate::db::skill_versions::save_version(
                db,
                skill,
                &content,
                "rename",
                None,
                Some("curator:rename"),
            )
            .await;
            tokio::fs::write(skills_dir.join(format!("{safe_new}.md")), &updated).await?;
            tokio::fs::remove_file(&old_path).await?;
            Ok(format!("RENAME {skill} -> {new_name}: {reason}"))
        }
    }
}

// ── Frontmatter helpers ───────────────────────────────────────────────────────

/// Replace (or insert) the `state:` field inside YAML frontmatter.
fn update_state_in_frontmatter(content: &str, state: &str) -> String {
    let trailing_nl = if content.ends_with('\n') { "\n" } else { "" };

    if content.contains("state:") {
        let mut opened = false;
        let mut closed = false;
        let lines: Vec<String> = content
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
                if opened && !closed && line.trim_start().starts_with("state:") {
                    return format!("state: {state}");
                }
                line.to_string()
            })
            .collect();
        lines.join("\n") + trailing_nl
    } else {
        // Insert before the closing `---` of the frontmatter.
        let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
        if let Some((pos, _)) = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, l)| l.trim() == "---")
        {
            lines.insert(pos, format!("state: {state}"));
        }
        lines.join("\n") + trailing_nl
    }
}

// ── LLM helper ────────────────────────────────────────────────────────────────

async fn llm_call(provider: &Arc<dyn LlmProvider>, prompt: &str) -> anyhow::Result<String> {
    use hydeclaw_types::{Message, MessageRole};

    let msg = Message {
        role: MessageRole::User,
        content: prompt.to_string(),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    };
    let resp = provider
        .chat(&[msg], &[], crate::agent::providers::CallOptions::default())
        .await
        .map_err(|e| anyhow::anyhow!("LLM call failed: {e}"))?;
    Ok(resp.content)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commands_skip() {
        let cmds = parse_commands(r#"[{"op":"SKIP"}]"#);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], CuratorCommand::Skip));
    }

    #[test]
    fn parse_commands_invalid_json_returns_empty() {
        let cmds = parse_commands("not json");
        assert!(cmds.is_empty());
    }

    #[test]
    fn parse_commands_skips_invalid_entries() {
        let cmds = parse_commands(r#"[{"op":"SKIP"},{"op":"UNKNOWN"}]"#);
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn validate_archive_pinned_fails() {
        let cmd = CuratorCommand::Archive {
            skill: "pinned-skill".into(),
            reason: "old".into(),
        };
        let err = validate_command(
            &cmd,
            &["pinned-skill".into()],
            &["pinned-skill".into()],
        );
        assert!(err.is_some());
    }

    #[test]
    fn validate_merge_less_than_two_sources_fails() {
        let cmd = CuratorCommand::Merge {
            sources: vec!["a".into()],
            into: "c".into(),
            reason: "test".into(),
        };
        let err = validate_command(&cmd, &["a".into(), "b".into()], &[]);
        assert!(err.is_some());
    }

    #[test]
    fn validate_archive_missing_skill_fails() {
        let cmd = CuratorCommand::Archive {
            skill: "ghost".into(),
            reason: "old".into(),
        };
        let err = validate_command(&cmd, &["other".into()], &[]);
        assert!(err.is_some());
    }

    #[test]
    fn validate_rename_existing_target_fails() {
        let cmd = CuratorCommand::Rename {
            skill: "a".into(),
            new_name: "b".into(),
            reason: "clash".into(),
        };
        let err = validate_command(&cmd, &["a".into(), "b".into()], &[]);
        assert!(err.is_some());
    }

    #[test]
    fn validate_valid_archive_passes() {
        let cmd = CuratorCommand::Archive {
            skill: "old".into(),
            reason: "unused".into(),
        };
        let err = validate_command(&cmd, &["old".into()], &[]);
        assert!(err.is_none());
    }

    #[test]
    fn update_state_replaces_existing_state_in_frontmatter() {
        let content = "---\nname: test\nstate: active\n---\n\nBody.\n";
        let updated = update_state_in_frontmatter(content, "archived");
        assert!(updated.contains("state: archived"));
        assert!(!updated.contains("state: active"));
        // Body should be untouched
        assert!(updated.contains("Body."));
    }

    #[test]
    fn update_state_inserts_missing_state_field() {
        let content = "---\nname: test\n---\n\nBody.\n";
        let updated = update_state_in_frontmatter(content, "archived");
        assert!(updated.contains("state: archived"));
        assert!(updated.contains("Body."));
    }
}
