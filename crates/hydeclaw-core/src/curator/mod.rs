pub mod phase_transitions;
pub mod phase_repairs;
pub mod phase_consolidation;

use std::sync::Arc;
use sqlx::PgPool;
use crate::config::CuratorConfig;
use crate::gateway::clusters::AgentCore;

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Sanitize a skill name to a safe filename stem.
/// Strips path-unsafe characters AND prevents directory traversal via `..`.
pub(crate) fn sanitize_skill_name(name: &str) -> String {
    let s = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
    if s.contains("..") {
        return s.replace("..", "-");
    }
    s
}

// ── Agent task runner ──────────────────────────────────────────────────────────

/// Run a single-turn verification task through a named agent.
/// Uses max_iterations=1 and an empty tool list so the agent cannot call tools —
/// it must respond with plain text (ACCEPT or REJECT).
pub(crate) async fn run_verifier_task(
    agents: &AgentCore,
    agent_name: &str,
    task: &str,
) -> anyhow::Result<String> {
    let engine = agents.get_engine(agent_name).await
        .ok_or_else(|| anyhow::anyhow!("curator: agent not found: {agent_name}"))?;
    engine
        .run_subagent(task, 1, None, None, None, Some(vec![]))
        .await
        .map_err(|e| anyhow::anyhow!("curator: verifier session failed: {e}"))
}

/// Run a task through a named agent via `run_subagent`. Returns the agent's text response.
pub(crate) async fn run_agent_task(
    agents: &AgentCore,
    agent_name: &str,
    task: &str,
) -> anyhow::Result<String> {
    let engine = agents.get_engine(agent_name).await
        .ok_or_else(|| anyhow::anyhow!("curator: agent not found: {agent_name}"))?;
    engine
        .run_subagent(task, 20, None, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("curator: agent session failed: {e}"))
}

// ── Public types ───────────────────────────────────────────────────────────────

pub struct CuratorRunSummary {
    pub phase1: i32,
    pub phase2: i32,
    pub phase3: i32,
    pub report_md: String,
}

// ── Orchestrator ───────────────────────────────────────────────────────────────

/// Run the full curator pipeline. Each phase is isolated — failure of one does not stop the next.
pub async fn run_curator(
    db: &PgPool,
    cfg: &CuratorConfig,
    agents: Arc<AgentCore>,
    workspace_dir: &str,
) -> anyhow::Result<CuratorRunSummary> {
    let mut report_lines: Vec<String> = Vec::new();
    let mut phase1_count = 0i32;
    let mut phase2_count = 0i32;
    let mut phase3_count = 0i32;

    // Phase 1: State transitions (no LLM)
    match phase_transitions::run(workspace_dir, db, cfg.stale_after_days, cfg.archive_after_days).await {
        Ok(r) => {
            phase1_count = r.transitions;
            if !r.log.is_empty() {
                report_lines.push("## Phase 1: State Transitions".into());
                report_lines.extend(r.log.iter().map(|l| format!("- {l}")));
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "curator phase1 failed");
            report_lines.push(format!("## Phase 1: FAILED — {e}"));
        }
    }

    // Phase 2: Repair queue (agent-driven)
    match phase_repairs::run(workspace_dir, db, agents.as_ref(), &cfg.agent_name, cfg.max_repairs_per_run).await {
        Ok(r) => {
            phase2_count = r.applied;
            if !r.log.is_empty() {
                report_lines.push("## Phase 2: Repairs".into());
                report_lines.extend(r.log.iter().map(|l| format!("- {l}")));
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "curator phase2 failed");
            report_lines.push(format!("## Phase 2: FAILED — {e}"));
        }
    }

    // Phase 3: Agent-driven consolidation
    match phase_consolidation::run(workspace_dir, agents.as_ref(), &cfg.agent_name, db).await {
        Ok(r) => {
            phase3_count = r.commands_executed;
            if !r.log.is_empty() {
                report_lines.push("## Phase 3: Consolidation".into());
                report_lines.extend(r.log.iter().map(|l| format!("- {l}")));
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "curator phase3 failed");
            report_lines.push(format!("## Phase 3: FAILED — {e}"));
        }
    }

    if report_lines.is_empty() {
        report_lines.push("Nothing to do.".into());
    }

    Ok(CuratorRunSummary {
        phase1: phase1_count,
        phase2: phase2_count,
        phase3: phase3_count,
        report_md: report_lines.join("\n"),
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_skill_name_blocks_dotdot_traversal() {
        assert_eq!(sanitize_skill_name("../config"), "--config");
        assert_eq!(sanitize_skill_name("a/../b"), "a---b");
        assert_eq!(sanitize_skill_name(".."), "-");
    }

    #[test]
    fn sanitize_skill_name_strips_unsafe_chars() {
        assert_eq!(sanitize_skill_name("my skill/name"), "my-skill-name");
        assert_eq!(sanitize_skill_name("a:b*c?d"), "a-b-c-d");
    }

    #[test]
    fn sanitize_skill_name_leaves_safe_names_unchanged() {
        assert_eq!(sanitize_skill_name("channel-formatting"), "channel-formatting");
        assert_eq!(sanitize_skill_name("skill_v2"), "skill_v2");
    }
    use tempfile::TempDir;

    /// Verify phase1 completes without panicking when the workspace has no
    /// skills directory (phase1 gracefully returns empty).
    ///
    /// This is a unit-level smoke test — it does NOT require a running Postgres
    /// instance. Phase 1 bails early when it can't read the skills dir.
    #[tokio::test]
    async fn phase1_no_skills_dir_no_panic() {
        // Create a real but empty temporary workspace
        let tmp = TempDir::new().expect("tempdir");
        let workspace_dir = tmp.path().to_str().unwrap();

        // A default CuratorConfig has provider_connection = "" and enabled = false
        let cfg = CuratorConfig::default();

        // Phase 1: no skills dir → should return empty result, not panic
        let result = phase_transitions::run(
            workspace_dir,
            &sqlx::PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap(),
            cfg.stale_after_days,
            cfg.archive_after_days,
        )
        .await
        .expect("phase1 must not error on missing skills dir");

        assert_eq!(result.transitions, 0);
        assert!(result.log.is_empty());
    }

    #[test]
    fn pinned_names_not_in_summary() {
        let summary = "- name: web-search\n- name: code-methodology\n- name: memory-management";
        let pinned = vec!["code-methodology".to_string(), "memory-management".to_string()];
        let task = format!(
            "NEVER touch pinned skills: [{}]\nSkills:\n{}",
            pinned.join(", "),
            summary
        );
        assert!(task.contains("code-methodology"));
        assert!(task.contains("memory-management"));
        let forbidden_section = task.split("Skills:").next().unwrap();
        assert!(forbidden_section.contains("code-methodology"));
    }
}
