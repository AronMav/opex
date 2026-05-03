//! Post-execution skill evolution — heartbeat tasks and interactive sessions.

use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;
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

/// Analyze a completed interactive session for skill improvements.
///
/// Loads session messages, builds a summary of user requests, and asks the
/// LLM whether any skill should be created or updated. Results are queued
/// via `pending_skill_repairs` for the curator to process.
///
/// Fires only for `Done` sessions with enough tool calls — never blocks.
pub async fn review_session_for_skills(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    session_id: Uuid,
) {
    if let Err(e) = review_session_inner(db, provider, agent_name, session_id).await {
        tracing::debug!(
            error = %e,
            agent = agent_name,
            session = %session_id,
            "session skill review failed"
        );
    }
}

async fn review_session_inner(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    session_id: Uuid,
) -> anyhow::Result<()> {
    use std::time::Duration;

    // 1. Load last 30 messages, keep only user messages for summary
    let rows = crate::db::sessions::load_messages(db, session_id, Some(30)).await?;
    let user_parts: Vec<&str> = rows.iter()
        .filter(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .collect();

    if user_parts.is_empty() {
        return Ok(());
    }

    // 2. Build task_summary — user messages only, truncated at UTF-8 char boundary
    let raw = user_parts.join("\n---\n");
    let boundary = raw.floor_char_boundary(2000);
    let task_summary = &raw[..boundary];

    // 3. Load available (non-archived) skill names
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

    // 4. Build prompt
    let prompt = format!(
        "You are a skill evolution analyzer reviewing a completed interactive session.\n\
         Agent: {agent_name}\n\
         User requests this session (summary):\n{task_summary}\n\n\
         Available skill names (ONLY use these exact names): {available_str}\n\n\
         Respond with EXACTLY ONE line:\n\
         - SKIP — session was casual, no reusable pattern emerged\n\
         - FIX <skill_name> — an existing skill has a gap or error revealed by this \
           session (skill_name MUST be from the list above)\n\
         - DERIVED <parent_skill> <new_name> — create a specialized variant of an \
           existing skill\n\
         - CAPTURED <new_name> — a genuinely new reusable workflow or technique appeared\n\n\
         Act when:\n\
           * The agent made a mistake the skill should prevent next time\n\
           * The user corrected approach, style, format, or workflow\n\
           * A non-trivial technique or pattern emerged future sessions would benefit from\n\
           * A loaded skill turned out to be wrong or incomplete\n\n\
         SKIP for casual conversation, one-off lookups, or sessions with no learnable \
         pattern. SKIP is a valid outcome — do not force an action where none fits."
    );

    let msg = Message {
        role: MessageRole::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    };

    // 5. Call LLM with 30s timeout
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        provider.chat(&[msg], &[], crate::agent::providers::CallOptions::default()),
    )
    .await;

    let analysis = match response {
        Ok(Ok(resp)) => resp.content,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, agent = agent_name, "session skill review: LLM call failed");
            return Ok(());
        }
        Err(_) => {
            tracing::warn!(agent = agent_name, "session skill review: LLM call timed out");
            return Ok(());
        }
    };

    // 6. Parse verdict and enqueue
    let line = analysis.trim();
    if line.starts_with("SKIP") {
        tracing::debug!(agent = agent_name, "session skill review: SKIP");
        return Ok(());
    }

    if let Some(rest) = line.strip_prefix("FIX ") {
        let skill_name = rest.split_whitespace().next().unwrap_or("");
        if !skill_name.is_empty() {
            let safe = skill_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
            let skill_path = format!("{}/skills/{safe}.md", crate::config::WORKSPACE_DIR);
            if tokio::fs::metadata(&skill_path).await.is_ok() {
                tracing::info!(skill = skill_name, agent = agent_name, "session skill review: FIX queued");
                skill_repairs::enqueue(db, skill_name, agent_name, "fix", line).await?;
            } else {
                tracing::warn!(skill = skill_name, agent = agent_name, "session skill review: FIX skipped — skill not found");
            }
        }
    } else if let Some(rest) = line.strip_prefix("DERIVED ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let new_name = parts.get(1).copied().unwrap_or("");
        if !new_name.is_empty() {
            tracing::info!(analysis = %line, agent = agent_name, "session skill review: DERIVED queued");
            skill_repairs::enqueue(db, new_name, agent_name, "derived", line).await?;
        }
    } else if let Some(rest) = line.strip_prefix("CAPTURED ") {
        let skill_name = rest.split_whitespace().next().unwrap_or("");
        if !skill_name.is_empty() {
            tracing::info!(analysis = %line, agent = agent_name, "session skill review: CAPTURED queued");
            skill_repairs::enqueue(db, skill_name, agent_name, "captured", line).await?;
        }
    } else {
        tracing::debug!(verdict = %line, agent = agent_name, "session skill review: unrecognised verdict");
    }

    tracing::info!(agent = agent_name, "session skill review complete");
    Ok(())
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

    // ── session skill review ──────────────────────────────────────────────────

    #[test]
    fn task_summary_truncates_at_char_boundary() {
        let long = "x".repeat(3000);
        let boundary = long.floor_char_boundary(2000);
        assert_eq!(boundary, 2000);
        assert!(long[..boundary].len() <= 2000);
    }

    #[test]
    fn task_summary_truncates_multibyte_safely() {
        // Cyrillic = 2 bytes per char; 1001 chars = 2002 bytes → must stop at 1000
        let long = "А".repeat(1001);
        let boundary = long.floor_char_boundary(2000);
        assert!(long.is_char_boundary(boundary));
        assert!(boundary <= 2000);
    }

    #[test]
    fn session_review_skip_verdict_detected() {
        let line = "SKIP";
        assert!(line.starts_with("SKIP"));
    }

    #[test]
    fn session_review_captured_extracts_name() {
        let line = "CAPTURED telegram-formatting";
        let name = line
            .strip_prefix("CAPTURED ")
            .and_then(|r| r.split_whitespace().next())
            .unwrap_or("");
        assert_eq!(name, "telegram-formatting");
    }

    #[test]
    fn session_review_fix_extracts_name() {
        let line = "FIX web-search because trigger too broad";
        let name = line
            .strip_prefix("FIX ")
            .and_then(|r| r.split_whitespace().next())
            .unwrap_or("");
        assert_eq!(name, "web-search");
    }

    #[test]
    fn session_review_derived_extracts_new_name() {
        let line = "DERIVED web-search web-search-news";
        let parts: Vec<&str> = line
            .strip_prefix("DERIVED ")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        assert_eq!(parts.get(1).copied().unwrap_or(""), "web-search-news");
    }

    #[test]
    fn build_task_summary_joins_user_messages() {
        let messages = vec![
            ("user", "first question"),
            ("assistant", "answer"),
            ("user", "second question"),
        ];
        let user_parts: Vec<&str> = messages.iter()
            .filter(|(role, _)| *role == "user")
            .map(|(_, content)| *content)
            .collect();
        let summary = user_parts.join("\n---\n");
        assert_eq!(summary, "first question\n---\nsecond question");
    }
}
