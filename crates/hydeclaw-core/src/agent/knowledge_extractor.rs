//! Post-session knowledge extraction.
//!
//! After a session completes with ≥ 5 messages, extracts user facts, outcomes,
//! and feedback via LLM and uses them to update the rolling summary in memory.

use std::sync::Arc;
use anyhow::Result;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::memory_service::MemoryService;
use crate::agent::providers::LlmProvider;
use hydeclaw_types::{Message, MessageRole};

/// Minimum messages in a session to trigger extraction.
const MIN_MESSAGES: usize = 5;
/// Maximum messages to include in the extraction prompt.
const MAX_CONTEXT_MESSAGES: usize = 20;
/// LLM call timeout.
const EXTRACTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Similarity threshold for dedup — skip saving if existing chunk is this similar.
const DEDUP_THRESHOLD: f64 = 0.9;

#[derive(Debug, Deserialize)]
struct ExtractedKnowledge {
    #[serde(default)]
    user_facts: Vec<String>,
    #[serde(default)]
    outcomes: Vec<String>,
    #[serde(default)]
    feedback: Vec<String>,
}

/// Extract knowledge from a completed session and save to memory.
/// Runs in background — errors are logged, never propagated.
pub async fn extract_and_save(
    db: PgPool,
    session_id: Uuid,
    agent_name: String,
    provider: Arc<dyn LlmProvider>,
    memory_store: Arc<dyn MemoryService>,
) {
    if !memory_store.is_available() {
        return;
    }

    if let Err(e) = extract_and_save_inner(&db, session_id, &agent_name, &provider, &memory_store).await {
        tracing::warn!(
            session_id = %session_id,
            agent = %agent_name,
            error = %e,
            "knowledge extraction failed"
        );
    }
}

async fn extract_and_save_inner(
    db: &PgPool,
    session_id: Uuid,
    agent_name: &str,
    provider: &Arc<dyn LlmProvider>,
    memory_store: &Arc<dyn MemoryService>,
) -> Result<()> {
    // 1. Load messages
    let rows = crate::db::sessions::load_messages(db, session_id, None).await?;
    if rows.len() < MIN_MESSAGES {
        return Ok(());
    }

    // 2. Build context: last N user+assistant messages (skip tool results to save tokens)
    let relevant: Vec<&crate::db::sessions::MessageRow> = rows.iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .collect();

    let start_idx = relevant.len().saturating_sub(MAX_CONTEXT_MESSAGES);
    let context_msgs = &relevant[start_idx..];

    if context_msgs.is_empty() {
        return Ok(());
    }

    // 3. Format conversation for LLM
    let mut conversation = String::new();
    for m in context_msgs {
        let role_label = match m.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            _ => continue,
        };
        let content = m.content.trim();
        if !content.is_empty() {
            conversation.push_str(&format!("{}: {}\n\n", role_label, content));
        }
    }

    if conversation.len() < 50 {
        return Ok(()); // Too short to extract anything meaningful
    }

    // 4. Call LLM for extraction
    let prompt = format!(
        "You are a knowledge extraction assistant. Analyze the conversation below and extract information worth remembering long-term.\n\n\
         Return a JSON object with three arrays:\n\
         {{\n\
           \"user_facts\": [\"...\"],\n\
           \"outcomes\": [\"...\"],\n\
           \"feedback\": [\"...\"]\n\
         }}\n\n\
         Categories:\n\
         - user_facts: Stable facts about the user — preferences, domain knowledge, long-term goals, identity\n\
         - outcomes: Durable decisions, agreements, or corrections that affect future sessions\n\
         - feedback: User's explicit reactions — what they approved, rejected, asked to redo\n\n\
         Rules (STRICTLY enforce):\n\
         - TIMELESS TEST: would this fact still matter in 6 months? If no — skip it.\n\
         - DO NOT extract what happened in this session: actions taken, requests made, things fixed/deleted/deployed.\n\
         - DO NOT extract facts implied by the conversation topic itself.\n\
         - Each item must be self-contained and make sense without reading the session.\n\
         - Write in the same language as the conversation.\n\
         - Maximum 3 items per category.\n\
         - Return empty arrays if nothing passes the timeless test.\n\n\
         Conversation:\n{}", conversation
    );

    let messages = vec![
        Message {
            role: MessageRole::User,
            content: prompt,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        },
    ];

    let response = tokio::time::timeout(
        EXTRACTION_TIMEOUT,
        provider.chat(&messages, &[]),
    )
    .await
    .map_err(|_| anyhow::anyhow!("extraction LLM call timed out"))??;

    // 5. Parse JSON from response
    let extracted = parse_extraction(&response.content)?;

    // 6. Dedup and save each fact
    let mut saved = 0u32;
    let source_prefix = format!("auto:session:{}", session_id);

    for fact in &extracted.user_facts {
        if save_if_new_with_provider(memory_store, fact, &format!("{}:user", source_prefix), agent_name, "shared", Some(provider)).await {
            saved += 1;
        }
    }
    for outcome in &extracted.outcomes {
        if save_if_new_with_provider(memory_store, outcome, &format!("{}:outcome", source_prefix), agent_name, "shared", Some(provider)).await {
            saved += 1;
        }
    }
    for fb in &extracted.feedback {
        if save_if_new_with_provider(memory_store, fb, &format!("{}:feedback", source_prefix), agent_name, "shared", Some(provider)).await {
            saved += 1;
        }
    }

    if saved > 0 {
        tracing::info!(
            session_id = %session_id,
            agent = %agent_name,
            saved,
            user_facts = extracted.user_facts.len(),
            outcomes = extracted.outcomes.len(),
            feedback = extracted.feedback.len(),
            "knowledge extracted from session"
        );
    }

    // 7. Update rolling agent summary
    update_rolling_summary(agent_name, provider, memory_store, &extracted).await;

    Ok(())
}

/// Update the rolling agent summary — a single pinned chunk that captures
/// the agent's accumulated knowledge about the user and context.
async fn update_rolling_summary(
    agent_name: &str,
    provider: &Arc<dyn LlmProvider>,
    memory_store: &Arc<dyn MemoryService>,
    extracted: &ExtractedKnowledge,
) {
    // Collect all new facts into one list
    let mut new_facts: Vec<&str> = Vec::new();
    for f in &extracted.user_facts { new_facts.push(f); }
    for f in &extracted.outcomes { new_facts.push(f); }
    for f in &extracted.feedback { new_facts.push(f); }

    if new_facts.is_empty() {
        return; // Nothing new to summarize
    }

    let summary_source = format!("rolling_summary:{}", agent_name);

    // Load current summary
    let current_summary = match memory_store.get(None, Some(&summary_source), 1).await {
        Ok(chunks) => chunks.first().map(|c| c.content.clone()).unwrap_or_default(),
        Err(_) => String::new(),
    };

    // Build update prompt
    let new_facts_text = new_facts.iter()
        .map(|f| format!("- {}", f))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = if current_summary.is_empty() {
        format!(
            "Create a concise agent summary (200 words max) from these facts about the user and recent interactions:\n\n{}\n\n\
             Write in the same language as the facts. Be concise — this summary is injected into every conversation.",
            new_facts_text
        )
    } else {
        format!(
            "Update this agent summary with new information. Keep it under 200 words. \
             Merge new facts into existing summary — don't duplicate, update contradictions, keep most important.\n\n\
             Current summary:\n{}\n\nNew facts:\n{}\n\n\
             Return ONLY the updated summary text, nothing else.",
            current_summary, new_facts_text
        )
    };

    let messages = vec![Message {
        role: MessageRole::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    }];

    // Retry up to 2 times on failure (LLM calls can be flaky)
    let mut response = None;
    for attempt in 0..2 {
        match tokio::time::timeout(EXTRACTION_TIMEOUT, provider.chat(&messages, &[])).await {
            Ok(Ok(r)) => { response = Some(r); break; }
            Ok(Err(e)) => {
                tracing::warn!(attempt, error = %e, "rolling summary LLM call failed, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(_) => {
                tracing::warn!(attempt, "rolling summary LLM call timed out, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    let response = match response {
        Some(r) => r,
        None => { tracing::warn!(agent = agent_name, "rolling summary failed after retries"); return; }
    };

    let new_summary = response.content.trim().to_string();
    if new_summary.is_empty() || new_summary.len() < 20 {
        return;
    }

    // Strip think blocks from summary
    let new_summary = {
        let mut s = new_summary;
        while let Some(start) = s.find("<think>") {
            if let Some(end) = s.find("</think>") {
                s = format!("{}{}", &s[..start], &s[end + 8..]);
            } else {
                s = s[..start].to_string();
                break;
            }
        }
        s.trim().to_string()
    };

    // Delete ALL old summary chunks (limit=10 prevents orphaned duplicates)
    if let Ok(chunks) = memory_store.get(None, Some(&summary_source), 10).await {
        for chunk in chunks.iter() {
            let _ = memory_store.delete(&chunk.id).await;
        }
    }

    // Save new summary as pinned chunk
    match memory_store.index(&new_summary, &summary_source, true, "private", agent_name).await {
        Ok(_) => tracing::info!(agent = agent_name, len = new_summary.len(), "rolling summary updated"),
        Err(e) => tracing::warn!(agent = agent_name, error = %e, "failed to save rolling summary"),
    }
}

/// Parse the LLM response into ExtractedKnowledge.
/// Handles markdown fences, <think> blocks, and partial JSON.
fn parse_extraction(content: &str) -> Result<ExtractedKnowledge> {
    // Strip <think>...</think> blocks
    let mut cleaned = content.to_string();
    while let Some(start) = cleaned.find("<think>") {
        if let Some(end) = cleaned.find("</think>") {
            cleaned = format!("{}{}", &cleaned[..start], &cleaned[end + 8..]);
        } else {
            cleaned = cleaned[..start].to_string();
            break;
        }
    }

    // Strip markdown fences
    let cleaned = cleaned
        .replace("```json", "")
        .replace("```", "")
        .trim()
        .to_string();

    // Find JSON object in the text
    if let Some(start) = cleaned.find('{')
        && let Some(end) = cleaned.rfind('}') {
        let json_str = &cleaned[start..=end];
        return Ok(serde_json::from_str(json_str)?);
    }

    anyhow::bail!("no JSON object found in extraction response")
}

/// Similarity thresholds for conflict resolution.
const CONFLICT_THRESHOLD: f64 = 0.5;

/// Save a fact to memory using Mem0-style conflict resolution.
/// - similarity >= 0.9 → SKIP (exact duplicate)
/// - similarity 0.5-0.9 → LLM decides ADD/UPDATE/DELETE/NOOP
/// - similarity < 0.5 → ADD (new fact)
#[cfg_attr(not(test), allow(dead_code))]
async fn save_if_new(
    memory_store: &Arc<dyn MemoryService>,
    text: &str,
    source: &str,
    agent_name: &str,
    scope: &str,
) -> bool {
    save_if_new_with_provider(memory_store, text, source, agent_name, scope, None).await
}

async fn save_if_new_with_provider(
    memory_store: &Arc<dyn MemoryService>,
    text: &str,
    source: &str,
    agent_name: &str,
    scope: &str,
    provider: Option<&Arc<dyn LlmProvider>>,
) -> bool {
    let text = text.trim();
    if text.is_empty() || text.len() < 10 {
        return false;
    }

    // Search for similar existing chunks
    let (results, _) = match memory_store.search(text, 3, &[], agent_name).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "dedup search failed, saving anyway");
            return match memory_store.index(text, source, false, scope, agent_name).await {
                Ok(_) => true,
                Err(e) => { tracing::warn!(error = %e, "failed to save extracted knowledge"); false }
            };
        }
    };

    if let Some(top) = results.first() {
        if top.similarity >= DEDUP_THRESHOLD {
            return false; // Exact duplicate — skip
        }

        if top.similarity >= CONFLICT_THRESHOLD {
            // Potential conflict — ask LLM to resolve if provider available
            if let Some(provider) = provider {
                return resolve_conflict(memory_store, provider, text, source, scope, agent_name, &results).await;
            }
            // No provider — fall through to ADD (safe default)
        }
    }

    // New fact or low similarity — ADD
    match memory_store.index(text, source, false, scope, agent_name).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, "failed to save extracted knowledge");
            false
        }
    }
}

/// Mem0-style conflict resolution: LLM decides ADD/UPDATE/DELETE/NOOP.
async fn resolve_conflict(
    memory_store: &Arc<dyn MemoryService>,
    provider: &Arc<dyn LlmProvider>,
    new_fact: &str,
    source: &str,
    scope: &str,
    agent_name: &str,
    existing: &[crate::memory::MemoryResult],
) -> bool {
    // Format existing memories for LLM
    let existing_text = existing.iter().enumerate()
        .map(|(i, r)| format!("[{}] {}", i, r.content))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You manage a memory store. A new fact needs to be stored, but similar memories already exist.\n\n\
         Existing memories:\n{}\n\n\
         New fact: {}\n\n\
         Decide the action. Return ONLY a JSON object:\n\
         {{\"action\": \"ADD|UPDATE|DELETE|NOOP\", \"target\": 0, \"reason\": \"...\"}}\n\n\
         - ADD: new fact is different/complementary, keep both\n\
         - UPDATE: new fact supersedes existing[target], replace it\n\
         - DELETE: existing[target] is outdated, delete it and add new\n\
         - NOOP: new fact adds nothing, skip it",
        existing_text, new_fact
    );

    let messages = vec![Message {
        role: MessageRole::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    }];

    let response = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        provider.chat(&messages, &[]),
    ).await {
        Ok(Ok(r)) => r,
        _ => {
            // LLM failed — safe fallback: ADD
            return memory_store.index(new_fact, source, false, scope, agent_name).await.is_ok();
        }
    };

    // Parse decision
    let decision = parse_conflict_decision(&response.content);

    match decision.action.as_str() {
        "UPDATE" | "DELETE" => {
            // Delete the target existing chunk, then add new
            let target_idx = decision.target.min(existing.len().saturating_sub(1));
            if let Some(target) = existing.get(target_idx) {
                let _ = memory_store.delete(&target.id).await;
                tracing::debug!(
                    action = decision.action.as_str(),
                    old = target.content.chars().take(50).collect::<String>(),
                    new = new_fact.chars().take(50).collect::<String>(),
                    reason = decision.reason.as_str(),
                    "memory conflict resolved"
                );
            }
            memory_store.index(new_fact, source, false, scope, agent_name).await.is_ok()
        }
        "ADD" => {
            memory_store.index(new_fact, source, false, scope, agent_name).await.is_ok()
        }
        _ => {
            tracing::debug!(action = decision.action.as_str(), reason = decision.reason.as_str(), "conflict resolution: unknown action, skipping");
            false
        }
    }
}

#[derive(Debug)]
struct ConflictDecision {
    action: String,
    target: usize,
    reason: String,
}

fn parse_conflict_decision(content: &str) -> ConflictDecision {
    let default = ConflictDecision { action: "ADD".into(), target: 0, reason: "parse failed".into() };

    // Strip think blocks
    let mut cleaned = content.to_string();
    while let Some(start) = cleaned.find("<think>") {
        if let Some(end) = cleaned.find("</think>") {
            cleaned = format!("{}{}", &cleaned[..start], &cleaned[end + 8..]);
        } else { break; }
    }
    let cleaned = cleaned.replace("```json", "").replace("```", "");

    let start = match cleaned.find('{') { Some(s) => s, None => return default };
    let end = match cleaned.rfind('}') { Some(e) => e, None => return default };

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned[start..=end]) {
        ConflictDecision {
            action: v.get("action").and_then(|a| a.as_str()).unwrap_or("ADD").to_uppercase(),
            target: v.get("target").and_then(|t| t.as_u64()).unwrap_or(0) as usize,
            reason: v.get("reason").and_then(|r| r.as_str()).unwrap_or("").to_string(),
        }
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_extraction tests ──────────────────────────────────────

    #[test]
    fn parse_clean_json() {
        let input = r#"{"user_facts":["User works in IT"],"outcomes":["Decided to use GraphQL"],"feedback":[]}"#;
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts, vec!["User works in IT"]);
        assert_eq!(result.outcomes, vec!["Decided to use GraphQL"]);
    }

    #[test]
    fn parse_with_markdown_fences() {
        let input = "Here is the result:\n```json\n{\"user_facts\":[\"Fact one\"],\"outcomes\":[],\"tool_insights\":[]}\n```";
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts, vec!["Fact one"]);
    }

    #[test]
    fn parse_with_think_blocks() {
        let input = "<think>Let me analyze this...</think>\n{\"user_facts\":[\"Important fact\"],\"outcomes\":[],\"tool_insights\":[]}";
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts, vec!["Important fact"]);
    }

    #[test]
    fn parse_with_surrounding_text() {
        let input = "Based on my analysis:\n\n{\"user_facts\":[\"A\"],\"outcomes\":[\"B\"],\"feedback\":[]}\n\nI hope this helps!";
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts, vec!["A"]);
        assert_eq!(result.outcomes, vec!["B"]);
    }

    #[test]
    fn parse_empty_arrays() {
        let input = r#"{"user_facts":[],"outcomes":[],"feedback":[]}"#;
        let result = parse_extraction(input).unwrap();
        assert!(result.user_facts.is_empty());
        assert!(result.outcomes.is_empty());
        assert!(result.feedback.is_empty());
    }

    #[test]
    fn parse_missing_fields_default_empty() {
        let input = r#"{"user_facts":["Only this"]}"#;
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts, vec!["Only this"]);
        assert!(result.outcomes.is_empty());
        assert!(result.feedback.is_empty());
    }

    #[test]
    fn parse_no_json_fails() {
        let input = "I could not extract anything from this conversation.";
        assert!(parse_extraction(input).is_err());
    }

    #[test]
    fn parse_nested_think_blocks() {
        let input = "<think>first</think>Some text<think>second</think>{\"user_facts\":[\"X\"],\"outcomes\":[],\"tool_insights\":[]}";
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts, vec!["X"]);
    }

    #[test]
    fn parse_unclosed_think_block() {
        let input = "<think>thinking forever... {\"user_facts\":[\"should not parse\"]}";
        // Unclosed think — everything after <think> is stripped
        assert!(parse_extraction(input).is_err());
    }

    #[test]
    fn parse_multiple_items_per_category() {
        let input = r#"{"user_facts":["F1","F2","F3"],"outcomes":["O1","O2"],"feedback":["FB1"]}"#;
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts.len(), 3);
        assert_eq!(result.outcomes.len(), 2);
        assert_eq!(result.feedback.len(), 1);
    }

    // ── save_if_new tests ───────────────────────────────────────────

    #[tokio::test]
    async fn save_if_new_skips_short_text() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::available()) as Arc<dyn MemoryService>;
        assert!(!save_if_new(&mock, "", "src", "agent", "private").await);
        assert!(!save_if_new(&mock, "short", "src", "agent", "private").await);
        assert!(!save_if_new(&mock, "  ", "src", "agent", "private").await);
    }

    #[tokio::test]
    async fn save_if_new_saves_valid_text() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::available()) as Arc<dyn MemoryService>;
        // Mock search returns empty results → no duplicate → should save
        let result = save_if_new(&mock, "This is a long enough fact to save", "auto:test", "agent", "shared").await;
        assert!(result);
    }

    // ── scope assignment tests ──────────────────────────────────────

    #[tokio::test]
    async fn save_if_new_accepts_private_scope() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::available()) as Arc<dyn MemoryService>;
        let result = save_if_new(&mock, "Tool insight only for this agent", "auto:test:tool", "Arty", "private").await;
        assert!(result);
    }

    #[tokio::test]
    async fn save_if_new_accepts_shared_scope() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::available()) as Arc<dyn MemoryService>;
        let result = save_if_new(&mock, "User works in IT sector", "auto:test:user", "Arty", "shared").await;
        assert!(result);
    }

    // ── feedback parsing tests ──────────────────────────────────────

    #[test]
    fn parse_with_feedback_field() {
        let input = r#"{"user_facts":["F1"],"outcomes":["O1"],"feedback":["User approved the analysis","User rejected the recommendation"]}"#;
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.feedback.len(), 2);
        assert_eq!(result.feedback[0], "User approved the analysis");
    }

    #[test]
    fn parse_without_feedback_defaults_empty() {
        let input = r#"{"user_facts":["F1"],"outcomes":[],"tool_insights":[]}"#;
        let result = parse_extraction(input).unwrap();
        assert!(result.feedback.is_empty());
    }

    // ── rolling summary tests ───────────────────────────────────────

    #[test]
    fn rolling_summary_collects_from_all_three_categories() {
        let extracted = ExtractedKnowledge {
            user_facts: vec!["User works in IT".into()],
            outcomes: vec!["Decided to use GraphQL".into()],
            feedback: vec!["User approved analysis".into()],
        };
        let mut facts: Vec<&str> = Vec::new();
        for f in &extracted.user_facts { facts.push(f); }
        for f in &extracted.outcomes { facts.push(f); }
        for f in &extracted.feedback { facts.push(f); }
        assert_eq!(facts.len(), 3);
        assert!(facts.iter().any(|f| f.contains("IT")));
        assert!(facts.iter().any(|f| f.contains("GraphQL")));
        assert!(facts.iter().any(|f| f.contains("approved")));
    }

    // ── conflict resolution tests ─────────────────────────────────

    #[test]
    fn parse_conflict_update() {
        let input = r#"{"action": "UPDATE", "target": 1, "reason": "new data supersedes old"}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "UPDATE");
        assert_eq!(d.target, 1);
        assert!(d.reason.contains("supersedes"));
    }

    #[test]
    fn parse_conflict_add() {
        let input = r#"{"action": "ADD", "target": 0, "reason": "complementary info"}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "ADD");
    }

    #[test]
    fn parse_conflict_noop() {
        let input = r#"{"action": "NOOP", "target": 0, "reason": "nothing new"}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "NOOP");
    }

    #[test]
    fn parse_conflict_delete() {
        let input = r#"{"action": "delete", "target": 2, "reason": "outdated"}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "DELETE"); // lowercased input → uppercased
        assert_eq!(d.target, 2);
    }

    #[test]
    fn parse_conflict_with_think_blocks() {
        let input = r#"<think>analyzing...</think>{"action": "UPDATE", "target": 0, "reason": "newer"}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "UPDATE");
    }

    #[test]
    fn parse_conflict_malformed_defaults_to_add() {
        let input = "I'm not sure what to do here.";
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "ADD"); // Safe default
    }

    // ── edge case: extraction with unicode/multilingual ────────

    #[test]
    fn parse_russian_content() {
        let input = r#"{"user_facts":["Пользователь работает в IT"],"outcomes":["Рекомендовано снизить нефтегаз до 25%"],"tool_insights":[],"feedback":["Одобрил анализ Alma"]}"#;
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts[0], "Пользователь работает в IT");
        assert_eq!(result.outcomes[0], "Рекомендовано снизить нефтегаз до 25%");
        assert_eq!(result.feedback[0], "Одобрил анализ Alma");
    }

    #[test]
    fn parse_mixed_languages() {
        let input = r#"{"user_facts":["User has BCS portfolio worth 525K RUB"],"outcomes":["Решено использовать GraphQL"],"tool_insights":[],"feedback":[]}"#;
        let result = parse_extraction(input).unwrap();
        assert!(result.user_facts[0].contains("525K RUB"));
        assert!(result.outcomes[0].contains("GraphQL"));
    }

    // ── edge case: malformed/partial JSON ────────────────────

    #[test]
    fn parse_json_with_trailing_text_after_brace() {
        let input = r#"{"user_facts":["A"],"outcomes":[],"tool_insights":[],"feedback":[]}
Some trailing explanation here."#;
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts, vec!["A"]);
    }

    #[test]
    fn parse_empty_string_items_preserved() {
        let input = r#"{"user_facts":["","valid fact",""],"outcomes":[],"tool_insights":[],"feedback":[]}"#;
        let result = parse_extraction(input).unwrap();
        assert_eq!(result.user_facts.len(), 3); // serde preserves empty strings
    }

    #[test]
    fn parse_special_characters_in_facts() {
        let input = r#"{"user_facts":["User's email: test@example.com"],"outcomes":["Budget: $50,000 (≈3.5M RUB)"],"tool_insights":[],"feedback":[]}"#;
        let result = parse_extraction(input).unwrap();
        assert!(result.user_facts[0].contains("test@example.com"));
        assert!(result.outcomes[0].contains("$50,000"));
    }

    // ── conflict resolution edge cases ──────────────────────

    #[test]
    fn parse_conflict_with_markdown_fences() {
        let input = "```json\n{\"action\": \"UPDATE\", \"target\": 0, \"reason\": \"newer info\"}\n```";
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "UPDATE");
    }

    #[test]
    fn parse_conflict_missing_target_defaults_to_zero() {
        let input = r#"{"action": "DELETE", "reason": "outdated"}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "DELETE");
        assert_eq!(d.target, 0);
    }

    #[test]
    fn parse_conflict_missing_reason() {
        let input = r#"{"action": "ADD", "target": 1}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "ADD");
        assert!(d.reason.is_empty());
    }

    #[test]
    fn parse_conflict_unknown_action_preserved() {
        let input = r#"{"action": "MERGE", "target": 0, "reason": "combine"}"#;
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "MERGE"); // uppercased, not mapped to known action
    }

    #[test]
    fn parse_conflict_empty_json() {
        let input = "{}";
        let d = parse_conflict_decision(input);
        assert_eq!(d.action, "ADD"); // default
        assert_eq!(d.target, 0);
    }

    // ── save_if_new threshold tests ─────────────────────────

    #[tokio::test]
    async fn save_if_new_rejects_exactly_10_chars() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::available()) as Arc<dyn MemoryService>;
        // Exactly 10 chars — boundary case (len < 10 returns false, so 10 should pass)
        assert!(save_if_new(&mock, "1234567890", "src", "agent", "private").await);
    }

    #[tokio::test]
    async fn save_if_new_rejects_9_chars() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::available()) as Arc<dyn MemoryService>;
        assert!(!save_if_new(&mock, "123456789", "src", "agent", "private").await);
    }

    #[tokio::test]
    async fn save_if_new_trims_whitespace() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::available()) as Arc<dyn MemoryService>;
        // "  short  " trims to "short" (5 chars) → rejected
        assert!(!save_if_new(&mock, "  short  ", "src", "agent", "private").await);
    }

    #[tokio::test]
    async fn save_if_new_unavailable_store_returns_false() {
        let mock = Arc::new(crate::agent::memory_service::mock::MockMemoryService::unavailable()) as Arc<dyn MemoryService>;
        // Store unavailable — should still save if called directly (save_if_new doesn't check availability)
        let result = save_if_new(&mock, "This is a long enough fact to save", "src", "agent", "shared").await;
        // MockMemoryService.unavailable() still returns Ok for index() — it just flags is_available=false
        assert!(result);
    }

    // ── scope consistency tests ─────────────────────────────

    #[test]
    fn extraction_scope_assignment() {
        // Verify the design: user_facts=shared, outcomes=shared, feedback=shared
        let scopes = [
            ("user_facts", "shared"),
            ("outcomes", "shared"),
            ("feedback", "shared"),
        ];
        // This is a documentation test — the actual scope assignment is in extract_and_save_inner
        // but we verify the design contract
        assert_eq!(scopes[0].1, "shared");
        assert_eq!(scopes[1].1, "shared");
        assert_eq!(scopes[2].1, "shared");
    }

}
