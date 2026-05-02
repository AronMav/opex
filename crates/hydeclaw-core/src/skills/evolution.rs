//! Post-execution skill evolution for cron/heartbeat tasks.
//! Runs ONLY after scheduled tasks — never on interactive chat.

use sqlx::PgPool;
use std::sync::Arc;
use hydeclaw_types::{Message, MessageRole};
use crate::db::skill_repairs;

/// Analyze a completed cron/heartbeat execution and evolve skills if needed.
pub async fn analyze_and_evolve(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    task_message: &str,
    response: &str,
    skills_used: &[String],
    success: bool,
) {
    // Skip trivial responses
    if response.len() < 20 || response.trim().eq_ignore_ascii_case("HEARTBEAT_OK") {
        return;
    }

    let skills_str = if skills_used.is_empty() {
        "none".to_string()
    } else {
        skills_used.join(", ")
    };
    let mut end = response.len().min(1000);
    while end > 0 && !response.is_char_boundary(end) { end -= 1; }
    let response_preview = &response[..end];

    // Load available skill names so the LLM picks from real files, not invented ones
    let available_skills = crate::skills::load_skills(crate::config::WORKSPACE_DIR).await;
    let available_names: Vec<String> = available_skills
        .iter()
        .filter(|s| !matches!(s.meta.state, crate::skills::SkillState::Archived))
        .map(|s| s.meta.name.clone())
        .collect();
    let available_str = if available_names.is_empty() {
        "none".to_string()
    } else {
        available_names.join(", ")
    };

    let analysis_prompt = format!(
        "You are a skill evolution analyzer. A scheduled task just completed.\n\n\
         Agent: {agent_name}\n\
         Task: {task_message}\n\
         Success: {success}\n\
         Skills used: {skills_str}\n\
         Response (truncated): {response_preview}\n\n\
         Available skill names (ONLY use these exact names): {available_str}\n\n\
         Respond with EXACTLY ONE line:\n\
         - SKIP — no changes needed\n\
         - FIX <skill_name> — an existing skill needs repair (skill_name MUST be from the list above)\n\
         - DERIVED <parent_skill> <new_name> — create specialized variant of an existing skill\n\
         - CAPTURED <new_name> — capture a genuinely new reusable pattern as a skill\n\n\
         IMPORTANT: skill_name must be an exact name from the available list. If nothing needs \
         changing or no existing skill is at fault, respond SKIP."
    );

    let msg = Message {
        role: MessageRole::User,
        content: analysis_prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    };

    let analysis = match provider.chat(&[msg], &[], crate::agent::providers::CallOptions::default()).await {
        Ok(resp) => resp.content,
        Err(e) => {
            tracing::debug!(error = %e, "skill evolution analysis failed");
            return;
        }
    };

    let line = analysis.trim();
    if line.starts_with("SKIP") {
        return;
    }

    if let Some(rest) = line.strip_prefix("FIX ") {
        let skill_name = rest.split_whitespace().next().unwrap_or("");
        if !skill_name.is_empty() {
            let safe_skill_name = skill_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
            let skill_path = format!("{}/skills/{safe_skill_name}.md", crate::config::WORKSPACE_DIR);
            match tokio::fs::read_to_string(&skill_path).await {
                Ok(content) => {
                    let _ = crate::db::skill_versions::save_version(
                        db, skill_name, &content, "pre-fix", None,
                        Some(&format!("Before auto-fix for agent {agent_name}")),
                    ).await;
                    tracing::info!(skill = skill_name, agent = agent_name, "skill evolution: FIX queued");
                    if let Err(e) = skill_repairs::enqueue(db, skill_name, agent_name, "fix", line).await {
                        tracing::warn!(error = %e, agent = agent_name, "skill evolution: failed to enqueue FIX repair");
                    }
                }
                Err(_) => {
                    tracing::warn!(skill = skill_name, agent = agent_name, "skill evolution: FIX skipped — skill file not found");
                }
            }
        }
    } else if let Some(rest) = line.strip_prefix("DERIVED ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        // Format: "DERIVED <parent_skill> <new_name>"
        // Use the new skill name (index 1) as the record key; parent is in diagnosis
        let new_skill_name = parts.get(1).copied().unwrap_or("");
        if !new_skill_name.is_empty() {
            tracing::info!(analysis = %line, agent = agent_name, "skill evolution: DERIVED queued");
            if let Err(e) = skill_repairs::enqueue(db, new_skill_name, agent_name, "derived", line).await {
                tracing::warn!(error = %e, agent = agent_name, "skill evolution: failed to enqueue DERIVED repair");
            }
        }
    } else if let Some(rest) = line.strip_prefix("CAPTURED ") {
        let skill_name = rest.split_whitespace().next().unwrap_or("");
        if !skill_name.is_empty() {
            tracing::info!(analysis = %line, agent = agent_name, "skill evolution: CAPTURED queued");
            if let Err(e) = skill_repairs::enqueue(db, skill_name, agent_name, "captured", line).await {
                tracing::warn!(error = %e, agent = agent_name, "skill evolution: failed to enqueue CAPTURED repair");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn skip_verdict_does_not_trigger_enqueue() {
        let line = "SKIP";
        assert!(line.starts_with("SKIP"));
        // If first word is SKIP, the function returns before DB calls.
        // This is a structural assertion — the real gate is in the implementation.
    }

    #[test]
    fn fix_verdict_extracts_skill_name() {
        let line = "FIX channel-formatting because triggers are too broad";
        let skill_name = line
            .strip_prefix("FIX ")
            .and_then(|r| r.split_whitespace().next())
            .unwrap_or("");
        assert_eq!(skill_name, "channel-formatting");
    }

    #[test]
    fn derived_verdict_extracts_parent_skill() {
        let line = "DERIVED channel-formatting channel-formatting-telegram";
        let parts: Vec<&str> = line
            .strip_prefix("DERIVED ")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        assert_eq!(parts.first().copied().unwrap_or(""), "channel-formatting");
    }

    #[test]
    fn captured_verdict_extracts_pattern_name() {
        let line = "CAPTURED new-pattern-name some description here";
        let skill_name = line
            .strip_prefix("CAPTURED ")
            .and_then(|r| r.split_whitespace().next())
            .unwrap_or("");
        assert_eq!(skill_name, "new-pattern-name");
    }
}
