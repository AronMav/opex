use std::sync::Arc;
use crate::agent::providers::LlmProvider;
use crate::db::skill_repairs::SkillRepairRow;

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
    provider: &Arc<dyn LlmProvider>,
    max_repairs: u32,
) -> anyhow::Result<RepairResult> {
    let repairs = crate::db::skill_repairs::list_pending(db, i64::from(max_repairs)).await?;
    let mut result = RepairResult { applied: 0, log: Vec::new() };

    for repair in repairs {
        match apply_repair(workspace_dir, db, provider, &repair).await {
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
    provider: &Arc<dyn LlmProvider>,
    repair: &SkillRepairRow,
) -> anyhow::Result<String> {
    let skills_dir = std::path::Path::new(workspace_dir).join("skills");
    let safe_name = repair.skill_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");

    match repair.kind.as_str() {
        "fix" => {
            let path = skills_dir.join(format!("{safe_name}.md"));
            let existing = tokio::fs::read_to_string(&path).await
                .map_err(|_| anyhow::anyhow!("skill file not found: {}", repair.skill_name))?;

            let prompt = format!(
                "Apply this fix to the skill body below. Return ONLY the updated skill body (everything after the frontmatter `---`).\n\nFix: {}\n\nCurrent skill:\n{}",
                repair.diagnosis, existing
            );
            let new_body = llm_call(provider, &prompt).await?;

            let fm_end = existing[3..].find("\n---").map(|i| i + 7).unwrap_or(existing.len());
            let new_content = format!("{}\n{}", &existing[..fm_end], new_body.trim());

            let _ = crate::db::skill_versions::save_version(db, &repair.skill_name, &existing, "repair", None, Some("curator:repair:fix")).await;
            tokio::fs::write(&path, &new_content).await?;
            Ok(repair.skill_name.clone())
        }

        "derived" => {
            let parent = extract_parent_from_diagnosis(&repair.diagnosis)
                .ok_or_else(|| anyhow::anyhow!("cannot extract parent from diagnosis"))?;
            let new_name = extract_new_name_from_derived(&repair.diagnosis)
                .unwrap_or(&repair.skill_name);
            let parent_safe = parent.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
            let parent_path = skills_dir.join(format!("{parent_safe}.md"));
            let parent_content = tokio::fs::read_to_string(&parent_path).await
                .map_err(|_| anyhow::anyhow!("parent skill not found: {parent}"))?;

            let prompt = format!(
                "Create a specialized variant skill named '{}' based on this parent skill. Return a complete skill file including YAML frontmatter (---) and instructions.\n\nParent skill:\n{}\n\nRequirement: {}",
                new_name, parent_content, repair.diagnosis
            );
            let new_skill_content = llm_call(provider, &prompt).await?;

            let new_safe = new_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
            tokio::fs::write(skills_dir.join(format!("{new_safe}.md")), &new_skill_content).await?;
            let _ = crate::db::skill_versions::save_version(db, new_name, &new_skill_content, "repair", None, Some("curator:repair:derived")).await;
            Ok(new_name.to_string())
        }

        "captured" => {
            let prompt = format!(
                "Create a new skill file named '{}' that captures this pattern. Return a complete skill file including YAML frontmatter (---) and instructions.\n\nPattern: {}",
                repair.skill_name, repair.diagnosis
            );
            let new_skill_content = llm_call(provider, &prompt).await?;

            tokio::fs::write(skills_dir.join(format!("{safe_name}.md")), &new_skill_content).await?;
            let _ = crate::db::skill_versions::save_version(db, &repair.skill_name, &new_skill_content, "repair", None, Some("curator:repair:captured")).await;
            Ok(repair.skill_name.clone())
        }

        other => anyhow::bail!("unknown repair kind: {other}"),
    }
}

async fn llm_call(provider: &Arc<dyn LlmProvider>, prompt: &str) -> anyhow::Result<String> {
    use crate::agent::providers::CallOptions;
    let msg = hydeclaw_types::Message {
        role: hydeclaw_types::MessageRole::User,
        content: prompt.to_string(),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    };
    let resp = provider.chat(&[msg], &[], CallOptions::default()).await
        .map_err(|e| anyhow::anyhow!("LLM call failed: {e}"))?;
    Ok(resp.content)
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
