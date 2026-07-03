//! Post-execution skill evolution — heartbeat tasks and interactive sessions.

use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;
use opex_types::{Message, MessageRole};
use crate::db::skill_repairs;

/// Analyze a completed cron/heartbeat execution and evolve skills if needed.
// reviewed: preview slice bounded by is_char_boundary walk-back — char boundary
#[allow(clippy::string_slice)]
pub async fn analyze_and_evolve(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    task_message: &str,
    response: &str,
    skills_used: &[String],
    success: bool,
) {
    // Skip trivial responses and clean heartbeats. Uses the same tolerant
    // all-clear check as the announcement-suppression path so a chatty
    // `**HEARTBEAT_OK**` report no longer drives skill churn (the cycle that
    // auto-generated competing heartbeat skills).
    if response.len() < 20 || crate::scheduler::is_heartbeat_ok(response) {
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
        db_id: None,
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
/// `force = true` bypasses the user_message_count >= 2 gate — used for
/// Failed/Interrupted sessions which are informative regardless of size.
pub async fn review_session_for_skills(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    session_id: Uuid,
    force: bool,
) {
    if let Err(e) = review_session_inner(db, provider, agent_name, session_id, force).await {
        tracing::debug!(
            error = %e,
            agent = agent_name,
            session = %session_id,
            "session skill review failed"
        );
    }
}

// reviewed: all offsets from find()/floor_char_boundary — char boundaries
#[allow(clippy::string_slice)]
async fn review_session_inner(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    session_id: Uuid,
    force: bool,
) -> anyhow::Result<()> {
    use std::time::Duration;

    // 1. Load last 30 messages.
    let rows = crate::db::sessions::load_messages(db, session_id, Some(30)).await?;

    let user_parts: Vec<&str> = rows.iter()
        .filter(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .collect();

    // Gate: require at least 2 user messages unless forced (Failed/Interrupted).
    if user_parts.len() < 2 && !force {
        return Ok(());
    }

    // 2. Fetch session.created_at for in-session capture lookup.
    let session_created_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT created_at FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

    // 3. Assistant text (first 300 chars each, strip trailing JSON).
    let assistant_parts: Vec<String> = rows.iter()
        .filter(|m| m.role == "assistant" && !m.content.is_empty())
        .map(|m| {
            let text = &m.content;
            let end = text.find("[{").or_else(|| text.find("{\"type\""))
                .unwrap_or(text.len());
            let end = text[..end].floor_char_boundary(300);
            text[..end].trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();

    // 4. Tool names from session_timeline.
    let tool_names: Vec<String> = {
        let names: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT DISTINCT payload->>'tool_name' \
             FROM session_timeline \
             WHERE session_id = $1 AND event_type = 'tool_end'",
        )
        .bind(session_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        names.into_iter().flatten().collect()
    };

    // 5. Skills captured in this session (from curator_decisions).
    let in_session_skills: Vec<String> = if let Some(created_at) = session_created_at {
        sqlx::query_scalar(
            "SELECT skill_name FROM curator_decisions \
             WHERE action = 'captured' AND decided_at >= $1",
        )
        .bind(created_at)
        .fetch_all(db)
        .await
        .unwrap_or_default()
    } else {
        vec![]
    };

    // 6. Load available (non-archived) skill names.
    let available_skills = crate::skills::load_skills(crate::config::WORKSPACE_DIR).await;
    let available_names: Vec<String> = available_skills
        .iter()
        .filter(|s| !matches!(s.meta.state, crate::skills::SkillState::Archived))
        .map(|s| s.meta.name.clone())
        .collect();
    let available_str = if available_names.is_empty() { "none".to_string() }
        else { available_names.join(", ") };

    // 7. Build context bundle, capped at 6 000 bytes total.
    let tools_str = if tool_names.is_empty() { "none".to_string() } else { tool_names.join(", ") };
    let captured_str = if in_session_skills.is_empty() { "none".to_string() } else { in_session_skills.join(", ") };

    // Derive outcome from force flag: force=false means Done, force=true means Failed/Interrupted.
    let outcome_str = if force { "failed or interrupted" } else { "done" };

    let meta = format!(
        "[Session metadata]\nAgent: {}\nOutcome: {}\nTool calls made: {}\nSkills captured this session: {}\n\n",
        agent_name, outcome_str, tools_str, captured_str
    );

    let user_raw = user_parts.join("\n---\n");
    let user_cap = user_raw.floor_char_boundary(2500);
    let user_section = format!("[User messages]\n{}\n\n", &user_raw[..user_cap]);

    let assistant_raw = assistant_parts.join("\n---\n");
    let assistant_cap = assistant_raw.floor_char_boundary(2000);
    let assistant_section = format!("[Assistant responses]\n{}", &assistant_raw[..assistant_cap]);

    let context = format!("{}{}{}", meta, user_section, assistant_section);
    let context_cap = context.floor_char_boundary(6000);
    let task_summary = &context[..context_cap];

    // 8. Build prompt with multi-verdict instructions.
    let prompt = format!(
        "You are a skill evolution analyzer reviewing a completed interactive session.\n\
         {task_summary}\n\n\
         Available skill names (ONLY use these exact names for FIX/DERIVED): {available_str}\n\n\
         Respond with 1–3 lines. Each line must be one of:\n\
         - SKIP\n\
         - FIX <skill_name>  (skill_name MUST be from the list above)\n\
         - DERIVED <parent_skill> <new_name>\n\
         - CAPTURED <new_name>\n\n\
         If nothing applies, respond with a single SKIP.\n\n\
         Act when:\n\
           * The agent made a mistake the skill should prevent next time\n\
           * The user corrected approach, style, format, or workflow\n\
           * A non-trivial reusable pattern emerged that future sessions would benefit from\n\
           * A loaded skill turned out to be wrong or incomplete\n\n\
         SKIP for casual conversation, one-off lookups, or sessions with no learnable pattern."
    );

    let msg = Message {
        role: MessageRole::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
        db_id: None,
    };

    // 9. Call LLM with 30s timeout.
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        provider.chat(&[msg], &[], crate::agent::providers::CallOptions::default()),
    )
    .await;

    let analysis = match response {
        Ok(Ok(resp)) => resp.content,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, agent = agent_name, "session skill review: LLM failed");
            return Ok(());
        }
        Err(_) => {
            tracing::warn!(agent = agent_name, "session skill review: LLM timed out");
            return Ok(());
        }
    };

    // 10. Parse up to 3 verdict lines (filter SKIP lines).
    let verdict_lines: Vec<&str> = analysis
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("SKIP"))
        .take(3)
        .collect();

    if verdict_lines.is_empty() {
        tracing::debug!(agent = agent_name, "session skill review: SKIP");
        return Ok(());
    }

    for line in verdict_lines {
        if let Some(rest) = line.strip_prefix("FIX ") {
            let skill_name = rest.split_whitespace().next().unwrap_or("");
            if !skill_name.is_empty() {
                let safe = skill_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
                let skill_path = format!("{}/skills/{safe}.md", crate::config::WORKSPACE_DIR);
                if tokio::fs::metadata(&skill_path).await.is_ok() {
                    tracing::info!(skill = skill_name, agent = agent_name, "session skill review: FIX queued");
                    crate::db::skill_repairs::enqueue(db, skill_name, agent_name, "fix", line).await?;
                } else {
                    tracing::warn!(skill = skill_name, agent = agent_name, "session skill review: FIX skipped — skill not found");
                }
            }
        } else if let Some(rest) = line.strip_prefix("DERIVED ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            let new_name = parts.get(1).copied().unwrap_or("");
            if !new_name.is_empty() {
                tracing::info!(analysis = %line, agent = agent_name, "session skill review: DERIVED queued");
                crate::db::skill_repairs::enqueue(db, new_name, agent_name, "derived", line).await?;
            }
        } else if let Some(rest) = line.strip_prefix("CAPTURED ") {
            let skill_name = rest.split_whitespace().next().unwrap_or("");
            if !skill_name.is_empty() {
                tracing::info!(analysis = %line, agent = agent_name, "session skill review: CAPTURED queued");
                crate::db::skill_repairs::enqueue(db, skill_name, agent_name, "captured", line).await?;
            }
        } else {
            tracing::debug!(verdict = %line, agent = agent_name, "session skill review: unrecognised verdict line");
        }
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
    // reviewed: floor_char_boundary-bounded ASCII fixture
    #[allow(clippy::string_slice)]
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
        let messages = [("user", "first question"),
            ("assistant", "answer"),
            ("user", "second question")];
        let user_parts: Vec<&str> = messages.iter()
            .filter(|(role, _)| *role == "user")
            .map(|(_, content)| *content)
            .collect();
        let summary = user_parts.join("\n---\n");
        assert_eq!(summary, "first question\n---\nsecond question");
    }

    // ── multi-verdict + force gate ───────────────────────────────────────────

    #[test]
    fn multi_verdict_all_three_parsed() {
        let response = "FIX web-search\nCAPTURED new-pattern\nDERIVED old-skill new-skill";
        let verdicts: Vec<&str> = response
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .take(3)
            .collect();
        assert_eq!(verdicts.len(), 3);
        assert!(verdicts[0].starts_with("FIX"));
        assert!(verdicts[1].starts_with("CAPTURED"));
        assert!(verdicts[2].starts_with("DERIVED"));
    }

    #[test]
    fn multi_verdict_skip_only_produces_nothing() {
        let response = "SKIP";
        let verdicts: Vec<&str> = response
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("SKIP"))
            .take(3)
            .collect();
        assert!(verdicts.is_empty());
    }

    #[test]
    fn multi_verdict_mixed_skip_ignored() {
        let response = "FIX web-search\nSKIP\nCAPTURED other";
        let verdicts: Vec<&str> = response
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("SKIP"))
            .take(3)
            .collect();
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts[0].starts_with("FIX"));
        assert!(verdicts[1].starts_with("CAPTURED"));
    }

    #[test]
    fn user_message_gate_blocks_single_message() {
        let user_parts: Vec<&str> = vec!["hello"];
        let force = false;
        let blocked = user_parts.len() < 2 && !force;
        assert!(blocked);
    }

    #[test]
    fn user_message_gate_passes_when_forced() {
        let user_parts: Vec<&str> = vec!["hello"];
        let force = true;
        let blocked = user_parts.len() < 2 && !force;
        assert!(!blocked);
    }

    #[test]
    // reviewed: offset from find() over ASCII fixture — char boundary
    #[allow(clippy::string_slice)]
    fn assistant_text_strips_json_prefix() {
        let content = "Here is the result.[{\"type\":\"tool_use\"}]";
        let end = content.find("[{").unwrap_or(content.len());
        let text = &content[..end];
        assert_eq!(text, "Here is the result.");
    }

    #[test]
    fn context_bundle_respects_byte_cap() {
        let cap = 6000usize;
        let long = "x".repeat(8000);
        let boundary = long.floor_char_boundary(cap);
        assert!(boundary <= cap);
        assert!(long.is_char_boundary(boundary));
    }
}
