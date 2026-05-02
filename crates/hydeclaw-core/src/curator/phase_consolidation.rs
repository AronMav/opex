//! Phase 3 — Agent-driven skill consolidation.
//!
//! Runs ONE Hyde agent session that analyses the active skill collection and
//! uses workspace tools to consolidate it directly (no JSON parsing, no
//! command enum, no fragile prompt templating).

use crate::gateway::clusters::AgentCore;

// ── Public result type ────────────────────────────────────────────────────────

pub struct ConsolidationResult {
    pub commands_executed: i32,
    pub log: Vec<String>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
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

    let task = format!(
        "[Curator: skill consolidation pass]\n\
         You are reviewing the active skill collection. Use your workspace tools \
         (workspace_read / workspace_write / workspace_edit) and skill-management \
         skills to consolidate where appropriate.\n\n\
         Rules:\n\
         - NEVER touch pinned skills: [{}]\n\
         - Do NOT delete skills — set state: archived in frontmatter instead\n\
         - Allowed actions: archive (set state), merge two-or-more sources into one new skill \
           (then archive sources), fix a skill body, rename a skill (rewrite filename + frontmatter name)\n\
         - Maximum 5 actions per pass\n\
         - Skip silently if nothing needs changing\n\n\
         Skills:\n{}\n\n\
         When done, reply with a short bullet list of the actions you took (one line each).",
        pinned.join(", "),
        summary
    );

    let report = crate::curator::run_agent_task(agents, agent_name, &task).await?;

    // commands_executed is always 0 — the agent's free-form report is the source of truth.
    Ok(ConsolidationResult {
        commands_executed: 0,
        log: report.lines().map(str::to_string).collect(),
    })
}
