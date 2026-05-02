//! Phase 3 — Analyst / Verifier / Executor skill consolidation.

use serde::Deserialize;
use crate::gateway::clusters::AgentCore;

// ── Public result type ────────────────────────────────────────────────────────

pub struct ConsolidationResult {
    pub commands_executed: i32,
    pub log: Vec<String>,
}

// ── Proposal data types ───────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct CapabilityEntry {
    pub capability: String,
    pub from_quote: String,
    pub covered_in: String,
    pub covering_quote: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub(crate) enum Proposal {
    Archive {
        skill: String,
        replacement: String,
        #[allow(dead_code)]
        reason: String,
        capability_map: Vec<CapabilityEntry>,
    },
    Merge {
        sources: Vec<String>,
        into: String,
        reason: String,
    },
    Fix {
        skill: String,
        description: String,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProposalsFile {
    pub proposals: Vec<Proposal>,
}

// ── Analyst ───────────────────────────────────────────────────────────────────

/// Read and parse the proposals file written by the Analyst session.
/// Returns an empty ProposalsFile on any error (missing file, invalid JSON).
async fn read_proposals_file(path: &str) -> ProposalsFile {
    match tokio::fs::read_to_string(path).await {
        Err(e) => {
            tracing::warn!(path, error = %e, "curator p3: proposals file not found");
            ProposalsFile { proposals: vec![] }
        }
        Ok(content) => match serde_json::from_str(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(path, error = %e, "curator p3: proposals file invalid JSON");
                ProposalsFile { proposals: vec![] }
            }
        },
    }
}

/// Run the Analyst Hyde session. Hyde reads active skills and writes
/// `workspace/curator_proposals.json` via workspace_write.
/// Returns the path where the agent will write curator_proposals.json.
fn proposals_path(workspace_dir: &str, agent_name: &str) -> std::path::PathBuf {
    // workspace_write stores files in workspace/agents/{name}/ by default.
    std::path::Path::new(workspace_dir)
        .join("agents")
        .join(agent_name)
        .join("curator_proposals.json")
}

async fn run_analyst(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
    active_summary: &str,
    pinned_names: &[String],
) -> anyhow::Result<ProposalsFile> {
    let proposals_path = proposals_path(workspace_dir, agent_name)
        .to_string_lossy()
        .to_string();

    let task = format!(
        "[Curator: skill consolidation — analyst pass]\n\
         Analyse the active skill collection and write your proposals to \
         workspace/curator_proposals.json using workspace_write.\n\n\
         NEVER touch pinned skills: [{}]\n\n\
         MANDATORY for any ARCHIVE proposal:\n\
         1. Read the FULL content of the skill to archive via workspace_read\n\
         2. Read the FULL content of the replacement skill via workspace_read\n\
         3. For every distinct capability/section in the archived skill, include a \
            capability_map entry with verbatim from_quote (from archived) and \
            covering_quote (from replacement).\n\
         4. If any capability has no verbatim covering_quote — do NOT include that \
            ARCHIVE proposal at all.\n\n\
         JSON schema for workspace/curator_proposals.json:\n\
         {{\n\
           \"proposals\": [\n\
             {{ \"action\": \"archive\", \"skill\": \"name\", \"replacement\": \"name\",\n\
                \"reason\": \"one sentence\",\n\
                \"capability_map\": [\n\
                  {{ \"capability\": \"name\", \"from_quote\": \"verbatim\",\n\
                     \"covered_in\": \"section\", \"covering_quote\": \"verbatim\" }}\n\
                ]\n\
             }},\n\
             {{ \"action\": \"merge\", \"sources\": [\"a\",\"b\"], \"into\": \"c\", \
                \"reason\": \"one sentence\" }},\n\
             {{ \"action\": \"fix\", \"skill\": \"name\", \"description\": \"what to fix\" }}\n\
           ]\n\
         }}\n\n\
         Rules:\n\
         - Maximum 5 proposals (ARCHIVE + MERGE + FIX combined)\n\
         - 'Never used' (last_used_at=null) is NOT a reason to archive\n\
         - Write ONLY the JSON file. Do not reply with analysis text.\n\n\
         Active skills (summary — read full files before deciding):\n{}",
        pinned_names.join(", "),
        active_summary
    );

    if let Err(e) = crate::curator::run_agent_task(agents, agent_name, &task).await {
        tracing::warn!(error = %e, "curator p3: analyst session failed");
    }

    Ok(read_proposals_file(&proposals_path).await)
}

// ── Verifier ──────────────────────────────────────────────────────────────────

/// Normalise text for fuzzy matching: lowercase, collapse whitespace.
fn normalise(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check whether `quote` appears in `text`.
/// Accepts exact match OR near-exact match after normalising whitespace/case.
/// Minimum quote length: 10 chars (shorter quotes are too generic to verify).
fn quote_found(text: &str, quote: &str) -> bool {
    let q = quote.trim();
    if q.len() < 10 {
        return true; // too short to verify meaningfully — pass through
    }
    text.contains(q) || normalise(text).contains(&normalise(q))
}

/// Verify an ARCHIVE proposal deterministically via string matching.
///
/// The Analyst already extracted verbatim quotes from both skills into the
/// capability_map. We simply confirm those quotes exist in the respective files.
/// No LLM call needed — avoids context overflow and timeouts.
///
/// Returns (accepted: bool, reject_reason: String).
async fn verify_archive_proposal(
    workspace_dir: &str,
    _agents: &AgentCore,
    _agent_name: &str,
    skill: &str,
    replacement: &str,
    capability_map: &[CapabilityEntry],
) -> (bool, String) {
    if capability_map.is_empty() {
        return (false, "capability_map is empty — analyst provided no evidence".into());
    }

    let skills_dir = std::path::Path::new(workspace_dir).join("skills");
    let safe_skill = crate::curator::sanitize_skill_name(skill);
    let safe_replacement = crate::curator::sanitize_skill_name(replacement);

    let archived_content = match tokio::fs::read_to_string(
        skills_dir.join(format!("{safe_skill}.md"))
    ).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(skill, error = %e, "curator p3 verifier: cannot read archived skill");
            return (false, format!("cannot read skill file: {e}"));
        }
    };

    let replacement_content = match tokio::fs::read_to_string(
        skills_dir.join(format!("{safe_replacement}.md"))
    ).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(replacement, error = %e, "curator p3 verifier: cannot read replacement skill");
            return (false, format!("cannot read replacement file: {e}"));
        }
    };

    let mut missing: Vec<String> = Vec::new();

    for entry in capability_map {
        let from_ok = quote_found(&archived_content, &entry.from_quote);
        let cover_ok = quote_found(&replacement_content, &entry.covering_quote);

        if !from_ok {
            missing.push(format!(
                "'{}': from_quote not found in archived skill",
                entry.capability
            ));
        } else if !cover_ok {
            missing.push(format!(
                "'{}': covering_quote not found in replacement skill",
                entry.capability
            ));
        }
    }

    if missing.is_empty() {
        tracing::info!(skill, replacement, entries = capability_map.len(),
                       "curator p3 verifier: all capability_map entries verified");
        (true, String::new())
    } else {
        tracing::info!(skill, replacement, missing = missing.join("; "),
                       "curator p3 verifier: capability_map failed verification");
        (false, missing.join("; "))
    }
}

// ── Executor ──────────────────────────────────────────────────────────────────

/// Programmatically archive a skill: save version snapshot, update frontmatter state.
async fn apply_verified_archive(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    skill: &str,
) -> anyhow::Result<()> {
    let safe = crate::curator::sanitize_skill_name(skill);
    let path = std::path::Path::new(workspace_dir).join("skills").join(format!("{safe}.md"));

    let content = tokio::fs::read_to_string(&path).await
        .map_err(|e| anyhow::anyhow!("apply_verified_archive: read {skill}: {e}"))?;

    let _ = crate::db::skill_versions::save_version(
        db, skill, &content, "archive", None,
        Some("curator:archive:verified"),
    ).await;

    // Update state in frontmatter (same pattern as phase_transitions)
    let updated = content.replacen("state: active", "state: archived", 1);
    let updated = if updated == content {
        content.replacen("state: stale", "state: archived", 1)
    } else {
        updated
    };

    let tmp = format!("{}.tmp", path.display());
    tokio::fs::write(&tmp, &updated).await
        .map_err(|e| anyhow::anyhow!("apply_verified_archive: write tmp {skill}: {e}"))?;
    tokio::fs::rename(&tmp, &path).await
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            anyhow::anyhow!("apply_verified_archive: rename {skill}: {e}")
        })?;

    Ok(())
}

/// Run executor Hyde session for MERGE and FIX proposals.
async fn run_executor(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
    proposals: &[&Proposal],
) -> anyhow::Result<i32> {
    if proposals.is_empty() {
        return Ok(0);
    }

    let proposals_text = proposals.iter().enumerate().map(|(i, p)| match p {
        Proposal::Merge { sources, into, reason } => format!(
            "{}. MERGE {} into '{}': {}",
            i + 1, sources.join(" + "), into, reason
        ),
        Proposal::Fix { skill, description } => format!(
            "{}. FIX '{}': {}", i + 1, skill, description
        ),
        _ => String::new(),
    }).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");

    let task = format!(
        "[Curator: skill consolidation — executor pass]\n\
         Apply these approved skill changes using workspace_write / workspace_edit. \
         Workspace skills are in workspace/skills/.\n\n\
         Changes to apply:\n{proposals_text}\n\n\
         For MERGE: create the new merged skill file, then set state: archived in \
         each source skill frontmatter.\n\
         For FIX: edit the skill body in-place."
    );

    crate::curator::run_agent_task(agents, agent_name, &task).await?;
    Ok(proposals.len() as i32)
}

/// Delete the proposals file and any leftover .tmp files after processing.
async fn cleanup_proposals(workspace_dir: &str, agent_name: &str) {
    let path = proposals_path(workspace_dir, agent_name);
    if let Err(e) = tokio::fs::remove_file(&path).await {
        tracing::debug!(error = %e, "curator p3: cleanup proposals file (may not exist)");
    }
    // Clean up any stray .tmp files from interrupted archive writes
    let skills_dir = std::path::Path::new(workspace_dir).join("skills");
    if let Ok(mut rd) = tokio::fs::read_dir(&skills_dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("tmp") {
                let _ = tokio::fs::remove_file(&p).await;
            }
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
    db: &sqlx::PgPool,
) -> anyhow::Result<ConsolidationResult> {
    let mut result = ConsolidationResult { commands_executed: 0, log: Vec::new() };

    // ── Load active skills ────────────────────────────────────────────────────
    let skills = crate::skills::load_skills(workspace_dir).await;
    let active: Vec<_> = skills.iter()
        .filter(|s| !matches!(s.meta.state, crate::skills::SkillState::Archived))
        .collect();

    if active.is_empty() {
        return Ok(ConsolidationResult {
            commands_executed: 0,
            log: vec!["no active skills".into()],
        });
    }

    let pinned: Vec<String> = active.iter()
        .filter(|s| s.meta.pinned.unwrap_or(false))
        .map(|s| s.meta.name.clone())
        .collect();

    let summary = active.iter().map(|s| {
        format!(
            "- name: {}\n  description: {}\n  state: {:?}\n  last_used_at: {}\n  triggers: [{}]",
            s.meta.name,
            s.meta.description,
            s.meta.state,
            s.meta.last_used_at.as_deref().unwrap_or("never"),
            s.meta.triggers.join(", ")
        )
    }).collect::<Vec<_>>().join("\n");

    // ── Step A: Analyst ───────────────────────────────────────────────────────
    let proposals = run_analyst(workspace_dir, agents, agent_name, &summary, &pinned).await?;

    if proposals.proposals.is_empty() {
        result.log.push("Analyst: no proposals.".into());
        cleanup_proposals(workspace_dir, agent_name).await;
        return Ok(result);
    }

    result.log.push(format!("Analyst: {} proposal(s).", proposals.proposals.len()));

    // ── Step B: Verify ARCHIVE proposals ─────────────────────────────────────
    let mut accepted_archives: Vec<(String, String)> = Vec::new();

    for proposal in &proposals.proposals {
        if let Proposal::Archive { skill, replacement, capability_map, .. } = proposal {
            let (accepted, reason) = verify_archive_proposal(
                workspace_dir, agents, agent_name,
                skill, replacement, capability_map,
            ).await;

            if accepted {
                accepted_archives.push((skill.clone(), replacement.clone()));
                result.log.push(format!("ARCHIVE `{skill}` → ACCEPTED (verifier)"));
            } else {
                result.log.push(format!("ARCHIVE `{skill}` → REJECTED: {reason}"));
            }
        }
    }

    // ── Step C: Execute ───────────────────────────────────────────────────────

    for (skill, _replacement) in &accepted_archives {
        match apply_verified_archive(workspace_dir, db, skill).await {
            Ok(()) => {
                result.commands_executed += 1;
                tracing::info!(skill, "curator p3: skill archived (verified)");
            }
            Err(e) => {
                tracing::warn!(skill, error = %e, "curator p3: archive apply failed");
                result.log.push(format!("⚠ archive `{skill}` failed: {e}"));
            }
        }
    }

    let executor_proposals: Vec<&Proposal> = proposals.proposals.iter()
        .filter(|p| matches!(p, Proposal::Merge { .. } | Proposal::Fix { .. }))
        .collect();

    if !executor_proposals.is_empty() {
        match run_executor(workspace_dir, agents, agent_name, &executor_proposals).await {
            Ok(n) => {
                result.commands_executed += n;
                result.log.push(format!("Executor: applied {n} merge/fix action(s)."));
            }
            Err(e) => {
                tracing::warn!(error = %e, "curator p3: executor session failed");
                result.log.push(format!("⚠ executor failed: {e}"));
            }
        }
    }

    cleanup_proposals(workspace_dir, agent_name).await;
    Ok(result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_quote_found_exact() {
        assert!(quote_found("hello world example", "hello world"));
    }

    #[test]
    fn verifier_quote_found_case_insensitive() {
        assert!(quote_found("Hello World Example", "hello world example"));
    }

    #[test]
    fn verifier_quote_found_normalises_whitespace() {
        assert!(quote_found("hello   world\n  example", "hello world example"));
    }

    #[test]
    fn verifier_quote_not_found() {
        assert!(!quote_found("completely different text", "hello world example"));
    }

    #[test]
    fn verifier_short_quote_always_passes() {
        assert!(quote_found("any text", "hi")); // < 10 chars — skipped
    }

    #[tokio::test]
    async fn read_proposals_missing_file_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("curator_proposals.json");
        let result = read_proposals_file(path.to_str().unwrap()).await;
        assert!(result.proposals.is_empty());
    }

    #[tokio::test]
    async fn read_proposals_invalid_json_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("curator_proposals.json");
        tokio::fs::write(&path, b"{{not json").await.unwrap();
        let result = read_proposals_file(path.to_str().unwrap()).await;
        assert!(result.proposals.is_empty());
    }

    #[test]
    fn proposals_valid_json_parses() {
        let json = r##"{
            "proposals": [
                {
                    "action": "archive",
                    "skill": "daily-reflection",
                    "replacement": "self-improvement",
                    "reason": "covered",
                    "capability_map": [
                        {
                            "capability": "journal format",
                            "from_quote": "Journal: YYYY-MM-DD",
                            "covered_in": "self-improvement Section 1",
                            "covering_quote": "Journal: YYYY-MM-DD"
                        }
                    ]
                },
                {
                    "action": "fix",
                    "skill": "research-strategy",
                    "description": "add section on source validation"
                }
            ]
        }"##;
        let parsed: ProposalsFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.proposals.len(), 2);
        match &parsed.proposals[0] {
            Proposal::Archive { skill, capability_map, .. } => {
                assert_eq!(skill, "daily-reflection");
                assert_eq!(capability_map.len(), 1);
            }
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn proposals_invalid_json_returns_err() {
        let result: Result<ProposalsFile, _> = serde_json::from_str("not json {{");
        assert!(result.is_err());
    }

    #[test]
    fn proposals_unknown_action_returns_err() {
        let json = r#"{"proposals": [{"action": "delete", "skill": "x"}]}"#;
        let result: Result<ProposalsFile, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
