use crate::db::skill_repairs::SkillRepairRow;
use crate::gateway::clusters::AgentCore;

pub struct RepairResult {
    pub applied: i32,
    pub log: Vec<String>,
}

/// Extract parent skill name from a DERIVED diagnosis string.
/// Input: "DERIVED channel-formatting channel-formatting-telegram some reason"
/// Output: Some("channel-formatting")
pub fn extract_parent_from_diagnosis(diagnosis: &str) -> Option<&str> {
    diagnosis
        .strip_prefix("DERIVED ")
        .and_then(|rest| rest.split_whitespace().next())
}

/// Extract new skill name (second token) from a DERIVED diagnosis.
pub fn extract_new_name_from_derived(diagnosis: &str) -> Option<&str> {
    let rest = diagnosis.strip_prefix("DERIVED ")?;
    let mut parts = rest.split_whitespace();
    parts.next()?; // skip parent
    parts.next()   // new name
}

pub async fn run(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    agents: &AgentCore,
    agent_name: &str,
    max_repairs: u32,
    dry_run: bool,
) -> anyhow::Result<RepairResult> {
    if dry_run {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pending_skill_repairs WHERE status = 'pending'",
        )
        .fetch_one(db)
        .await
        .unwrap_or(0);
        return Ok(RepairResult {
            applied: 0,
            log: vec![format!("[DRY-RUN] {} pending repairs (not executed)", count)],
        });
    }

    let repairs = crate::db::skill_repairs::list_pending(db, i64::from(max_repairs)).await?;
    let mut result = RepairResult { applied: 0, log: Vec::new() };

    for repair in repairs {
        match apply_repair(workspace_dir, db, agents, agent_name, &repair).await {
            Ok(name) => {
                crate::db::skill_repairs::resolve(db, repair.id, "done", None).await.ok();
                result.applied += 1;
                result.log.push(format!("repaired: {} ({})", name, repair.kind));
            }
            Err(e) => {
                let msg = e.to_string();
                crate::db::skill_repairs::fail(db, repair.id, &msg).await.ok();
                tracing::warn!(skill = %repair.skill_name, kind = %repair.kind, error = %msg, "phase2: repair failed");
            }
        }
    }

    Ok(result)
}

async fn apply_repair(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    agents: &AgentCore,
    agent_name: &str,
    repair: &SkillRepairRow,
) -> anyhow::Result<String> {
    let skills_dir = std::path::Path::new(workspace_dir).join("skills");
    let safe_name = crate::curator::sanitize_skill_name(&repair.skill_name);

    match repair.kind.as_str() {
        "fix" => {
            let path = skills_dir.join(format!("{safe_name}.md"));
            let existing = tokio::fs::read_to_string(&path).await
                .map_err(|_| anyhow::anyhow!("skill file not found: {}", repair.skill_name))?;

            // Snapshot the current version before handing off to Opex
            let _ = crate::db::skill_versions::save_version(
                db, &repair.skill_name, &existing, "repair", None, Some("curator:repair:fix"),
            ).await;

            let task = format!(
                "[Curator: skill repair — fix]\n\
                 Fix the skill '{}'. Use workspace_edit / workspace_write to apply the change \
                 in-place at workspace/skills/{}.md.\n\n\
                 Fix description: {}\n\n\
                 Current skill body for reference:\n{}",
                repair.skill_name, safe_name, repair.diagnosis, existing
            );
            crate::curator::run_agent_task(agents, agent_name, &task).await?;
            Ok(repair.skill_name.clone())
        }

        "derived" => {
            let parent = extract_parent_from_diagnosis(&repair.diagnosis)
                .ok_or_else(|| anyhow::anyhow!("cannot extract parent from diagnosis"))?;
            let new_name = extract_new_name_from_derived(&repair.diagnosis)
                .unwrap_or(&repair.skill_name);
            let parent_safe = crate::curator::sanitize_skill_name(parent);
            let new_safe = crate::curator::sanitize_skill_name(new_name);
            let parent_path = skills_dir.join(format!("{parent_safe}.md"));
            let parent_content = tokio::fs::read_to_string(&parent_path).await
                .map_err(|_| anyhow::anyhow!("parent skill not found: {parent}"))?;

            let task = format!(
                "[Curator: skill repair — derived]\n\
                 Create a new specialized skill named '{}' derived from parent '{}'. \
                 Use workspace_write to create workspace/skills/{}.md with full YAML frontmatter \
                 and instructions.\n\n\
                 Requirement: {}\n\n\
                 Parent skill body:\n{}",
                new_name, parent, new_safe, repair.diagnosis, parent_content
            );
            crate::curator::run_agent_task(agents, agent_name, &task).await?;

            // Try to snapshot the new file Opex just wrote; log warning if unavailable
            let new_path = skills_dir.join(format!("{new_safe}.md"));
            match tokio::fs::read_to_string(&new_path).await {
                Ok(content) => {
                    let _ = crate::db::skill_versions::save_version(
                        db, new_name, &content, "repair", None, Some("curator:repair:derived"),
                    ).await;
                }
                Err(e) => {
                    tracing::warn!(
                        skill = %new_name, error = %e,
                        "phase2: derived skill not found after agent write — skipping version snapshot"
                    );
                }
            }
            Ok(new_name.to_string())
        }

        "captured" => {
            let task = format!(
                "[Curator: skill repair — captured]\n\
                 Create a new skill named '{}' that captures this pattern. \
                 Use workspace_write to create workspace/skills/{}.md with full YAML frontmatter \
                 and instructions.\n\n\
                 Pattern: {}",
                repair.skill_name, safe_name, repair.diagnosis
            );
            crate::curator::run_agent_task(agents, agent_name, &task).await?;

            // Try to snapshot the new file Opex just wrote
            let new_path = skills_dir.join(format!("{safe_name}.md"));
            match tokio::fs::read_to_string(&new_path).await {
                Ok(content) => {
                    let _ = crate::db::skill_versions::save_version(
                        db, &repair.skill_name, &content, "repair", None, Some("curator:repair:captured"),
                    ).await;
                }
                Err(e) => {
                    tracing::warn!(
                        skill = %repair.skill_name, error = %e,
                        "phase2: captured skill not found after agent write — skipping version snapshot"
                    );
                }
            }
            Ok(repair.skill_name.clone())
        }

        other => anyhow::bail!("unknown repair kind: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_parent_from_derived_diagnosis() {
        assert_eq!(
            extract_parent_from_diagnosis("DERIVED channel-formatting channel-formatting-telegram some reason"),
            Some("channel-formatting")
        );
    }

    #[test]
    fn extract_new_name_from_derived_diagnosis() {
        assert_eq!(
            extract_new_name_from_derived("DERIVED channel-formatting channel-formatting-telegram"),
            Some("channel-formatting-telegram")
        );
    }

    #[test]
    fn extract_parent_returns_none_for_non_derived() {
        assert_eq!(extract_parent_from_diagnosis("FIX some-skill"), None);
    }

    #[test]
    fn extract_parent_returns_none_for_empty() {
        assert_eq!(extract_parent_from_diagnosis("DERIVED"), None);
    }
}
