//! Post-execution skill evolution for cron/heartbeat tasks.
//! Runs ONLY after scheduled tasks — never on interactive chat.

use sqlx::PgPool;
use std::sync::Arc;
use hydeclaw_types::{Message, MessageRole};

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

    // Only evolve on failures or unusually long responses
    if success && response.len() < 2000 {
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

    let analysis_prompt = format!(
        "You are a skill evolution analyzer. A scheduled task just completed.\n\n\
         Agent: {agent_name}\n\
         Task: {task_message}\n\
         Success: {success}\n\
         Skills used: {skills_str}\n\
         Response (truncated): {response_preview}\n\n\
         Respond with EXACTLY ONE line:\n\
         - SKIP — no changes needed\n\
         - FIX <skill_name> — skill needs repair, explain what's wrong\n\
         - DERIVED <parent_skill> <new_name> — create specialized variant\n\
         - CAPTURED <new_name> — new pattern detected\n\n\
         Be conservative."
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
        tracing::info!(skill = skill_name, agent = agent_name, "skill evolution: FIX suggested");
        if let Ok(content) = tokio::fs::read_to_string(
            format!("workspace/skills/{skill_name}.md")
        ).await {
            let _ = crate::db::skill_versions::save_version(
                db, skill_name, &content, "pre-fix", None,
                Some(&format!("Before auto-fix for agent {agent_name}")),
            ).await;
        }
        tracing::info!(analysis = %analysis, "FIX analysis recorded");
    } else if line.starts_with("DERIVED ") || line.starts_with("CAPTURED ") {
        tracing::info!(analysis = %analysis, agent = agent_name, "skill evolution suggestion recorded");
    }
}
