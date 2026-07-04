//! Pipeline step: context — build_context, compaction, tool result handling.
//! Migrated from engine_context.rs as free functions with explicit dependencies.

use anyhow::Result;
use std::sync::Arc;
use uuid::Uuid;

use opex_types::{Message, MessageRole};
use crate::agent::context_builder::{ContextBuilder, ContextSnapshot};
use crate::agent::engine::row_to_message;
use crate::agent::history;
use crate::agent::providers::LlmProvider;
use crate::agent::session_manager::SessionManager;
use crate::agent::tool_loop::LoopDetector;
use crate::config::CompactionConfig;
use crate::secrets::SecretsManager;
use crate::tools::yaml_tools::OAuthContext;

/// Delegate to ContextBuilder.
pub async fn build_context(
    context_builder: &dyn ContextBuilder,
    msg: &opex_types::IncomingMessage,
    include_tools: bool,
    resume_session_id: Option<Uuid>,
    force_new_session: bool,
) -> Result<ContextSnapshot> {
    context_builder
        .build(msg, include_tools, resume_session_id, force_new_session)
        .await
}

/// Build a `SecretsEnvResolver` for YAML tool env resolution.
pub fn make_resolver(
    secrets: &Arc<SecretsManager>,
    agent_name: &str,
) -> crate::agent::engine::SecretsEnvResolver {
    crate::agent::engine::SecretsEnvResolver {
        secrets: secrets.clone(),
        agent_name: agent_name.to_string(),
    }
}

/// Build `OAuthContext` for provider-based YAML tool auth.
pub fn make_oauth_context(
    oauth: Option<&Arc<crate::oauth::OAuthManager>>,
    agent_name: &str,
) -> Option<OAuthContext> {
    oauth.map(|mgr| OAuthContext {
        manager: mgr.clone(),
        agent_id: agent_name.to_string(),
    })
}

/// Format a tool error as structured JSON for better LLM parsing.
pub fn format_tool_error(tool_name: &str, error: &str) -> String {
    serde_json::json!({"status": "error", "tool": tool_name, "error": error}).to_string()
}

/// Truncate a string to `max` *Unicode scalar values* with "..." suffix.
///
/// Uses `char`-based counting so multi-byte codepoints (emoji, CJK, …) are
/// never split mid-sequence.  When `max < 4` there is no room for the
/// ellipsis, so the first `max` chars are returned as-is.
pub fn truncate_preview(s: &str, max: usize) -> String {
    // Count chars once — O(n) but n is bounded by callers (≤8 typical).
    let char_count = s.chars().count();
    if char_count <= max {
        return s.to_string();
    }
    if max < 4 {
        // No room for "..."; return the bare prefix.
        return s.chars().take(max).collect();
    }
    let prefix: String = s.chars().take(max - 3).collect();
    format!("{}...", prefix)
}

/// Truncate a tool result to fit within remaining context budget.
/// Preserves head + tail (tail may contain errors/JSON closing).
/// Budget: 50% of remaining context, floor 2000 chars.
// reviewed: all slice bounds via floor_char_boundary — char boundaries
#[allow(clippy::string_slice)]
pub fn truncate_tool_result(model: &str, result: &str, current_context_chars: usize) -> String {
    // Provider-resolved window (via the /api/show cache) — NOT the stale name
    // heuristic — so a 262k/1M model isn't truncated as if it were 128k.
    let model_max_chars = super::llm_call::context_limit_tokens(model) as usize * 4;
    let remaining = model_max_chars.saturating_sub(current_context_chars);
    let limit = (remaining * 50 / 100).max(2000);
    if result.len() <= limit {
        return result.to_string();
    }
    // Slice on a char boundary — a raw byte offset lands mid-codepoint for
    // multi-byte text (Cyrillic, CJK, emoji) and panics ("byte index is not a
    // char boundary"), which killed sessions on large ИТС (Russian) results.
    let tail_region = &result[result.floor_char_boundary(result.len().saturating_sub(1500))..];
    let tail_has_error = tail_region.contains("error")
        || tail_region.contains("Error")
        || tail_region.contains("failed")
        || tail_region.contains("exception");
    let tail_size = if tail_has_error { 1500 } else { 500 };
    let marker = format!(
        "\n\n[... truncated {} → {} chars ...]\n\n",
        result.len(),
        limit
    );
    let head_size = limit.saturating_sub(tail_size).saturating_sub(marker.len());
    let head = &result[..result.floor_char_boundary(head_size)];
    let tail = &result[result.floor_char_boundary(result.len().saturating_sub(tail_size))..];
    tracing::debug!(original = result.len(), truncated = limit, tail_has_error, "tool result truncated");
    format!("{}{}{}", head, marker, tail)
}

/// Replace old tool results with "[compacted]" when context exceeds 70% of model window.
/// Preserves the last `preserve_n` tool results and the system message.
pub fn compact_tool_results(
    model: &str,
    compaction_config: Option<&CompactionConfig>,
    messages: &mut [Message],
    context_chars: &mut usize,
) {
    let context_window = super::llm_call::context_limit_tokens(model) as usize * 4;
    let threshold = context_window * 70 / 100;
    if *context_chars <= threshold {
        return;
    }
    let preserve_n = compaction_config
        .map(|c| c.preserve_last_n as usize)
        .unwrap_or(10);

    let tool_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == MessageRole::Tool)
        .map(|(i, _)| i)
        .collect();
    let to_compact = tool_indices.len().saturating_sub(preserve_n);
    if to_compact == 0 {
        return;
    }

    let mut compacted = 0usize;
    let mut chars_removed = 0usize;
    for &idx in tool_indices.iter().take(to_compact) {
        let old_len = messages[idx].content.chars().count();
        if old_len > 100 {
            let replacement = "[tool result compacted]";
            let new_len = replacement.len();
            chars_removed += old_len - new_len;
            messages[idx].content = replacement.to_string();
            compacted += 1;
        }
    }
    if compacted > 0 {
        let old_total = *context_chars;
        *context_chars = context_chars.saturating_sub(chars_removed);
        tracing::info!(compacted, old_chars = old_total, new_chars = *context_chars, "compacted old tool results");
    }
}

/// Get compaction parameters from agent config.
pub fn compaction_params(
    model: &str,
    compaction_config: Option<&CompactionConfig>,
) -> (usize, usize) {
    let max_tokens = compaction_config
        .and_then(|c| c.max_context_tokens)
        .map(|t| t as usize)
        .unwrap_or_else(|| super::llm_call::context_limit_tokens(model) as usize);
    let preserve_last_n = compaction_config
        .map(|c| c.preserve_last_n as usize)
        .unwrap_or(10);
    (max_tokens, preserve_last_n)
}

/// Run compaction on messages if token budget exceeded, indexing extracted facts to memory.
/// Pass `Some(detector)` when inside the LLM loop to inject a progress header after compaction.
///
/// `index_facts` is a callback for writing extracted facts to memory (delegates to engine).
#[allow(clippy::too_many_arguments)]
pub async fn compact_messages<F, Fut>(
    model: &str,
    compaction_config: Option<&CompactionConfig>,
    language: &str,
    provider: &dyn LlmProvider,
    compaction_provider: Option<&dyn LlmProvider>,
    db: &sqlx::PgPool,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    agent_name: &str,
    _audit_queue: &crate::db::audit_queue::AuditQueue,
    messages: &mut Vec<Message>,
    detector: Option<&LoopDetector>,
    index_facts: F,
) where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (max_tokens, preserve_last_n) = compaction_params(model, compaction_config);
    if let Ok(Some(facts)) = history::compact_if_needed(
        messages,
        provider,
        compaction_provider,
        max_tokens,
        preserve_last_n,
        Some(language),
    )
    .await
    {
        tracing::info!(facts = facts.len(), "extracted facts during compaction");
        crate::db::audit::audit_spawn(
            db.clone(),
            agent_name.to_string(),
            crate::db::audit::event_types::COMPACTION,
            None,
            serde_json::json!({"facts": facts.len(), "max_tokens": max_tokens}),
        );
        index_facts(facts).await;

        // Inject / replace progress header after compaction
        if let Some(det) = detector {
            history::remove_progress_header(messages);
            let header = history::generate_progress_header(messages, det);
            let insert_pos = if messages
                .first()
                .map(|m| m.role == MessageRole::System)
                .unwrap_or(false)
            {
                1
            } else {
                0
            };
            messages.insert(
                insert_pos,
                Message {
                    role: MessageRole::System,
                    content: header,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking_blocks: vec![],
            db_id: None,
                },
            );
        }

        // Notify user about compaction
        if let Some(ui_tx) = ui_event_tx {
            let db = db.clone();
            let tx = ui_tx.clone();
            let agent_name = agent_name.to_string();
            // AUDIT-FF-014: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
            tokio::spawn(async move {
                crate::gateway::notify(
                    &db,
                    &tx,
                    "context_compaction",
                    &format!("Context compacted: {}", agent_name),
                    &format!(
                        "Agent {} session was compacted to stay within token budget",
                        agent_name
                    ),
                    serde_json::json!({"agent": agent_name}),
                )
                .await
                .ok();
            });
        }
    }
}

/// Force-compact messages regardless of the token-threshold gate, indexing
/// extracted facts to memory. Used by reactive context-overflow recovery
/// (`pipeline::llm_call::chat_stream_with_overflow_recovery` /
/// `chat_with_overflow_recovery`) — the provider already rejected the call
/// as too large, so waiting on the proactive gate (which estimates tokens
/// via a rough heuristic, see `history::estimate_tokens`) risks a no-op
/// compaction that leaves the retry doomed to fail identically.
///
/// Mirrors [`compact_messages`] (same fact-indexing / progress-header /
/// notification side effects) but calls `history::force_compact` instead of
/// the gated `history::compact_if_needed`.
#[allow(clippy::too_many_arguments)]
pub async fn compact_messages_force<F, Fut>(
    compaction_config: Option<&CompactionConfig>,
    language: &str,
    provider: &dyn LlmProvider,
    compaction_provider: Option<&dyn LlmProvider>,
    db: &sqlx::PgPool,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    agent_name: &str,
    messages: &mut Vec<Message>,
    detector: Option<&LoopDetector>,
    index_facts: F,
) where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let preserve_last_n = compaction_config
        .map(|c| c.preserve_last_n as usize)
        .unwrap_or(10);
    if let Ok(Some(facts)) = history::force_compact(
        messages,
        provider,
        compaction_provider,
        preserve_last_n,
        Some(language),
    )
    .await
    {
        tracing::info!(facts = facts.len(), "extracted facts during forced (overflow-recovery) compaction");
        crate::db::audit::audit_spawn(
            db.clone(),
            agent_name.to_string(),
            crate::db::audit::event_types::COMPACTION,
            None,
            serde_json::json!({"facts": facts.len(), "forced": true}),
        );
        index_facts(facts).await;

        if let Some(det) = detector {
            history::remove_progress_header(messages);
            let header = history::generate_progress_header(messages, det);
            let insert_pos = if messages
                .first()
                .map(|m| m.role == MessageRole::System)
                .unwrap_or(false)
            {
                1
            } else {
                0
            };
            messages.insert(
                insert_pos,
                Message {
                    role: MessageRole::System,
                    content: header,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking_blocks: vec![],
                    db_id: None,
                },
            );
        }

        if let Some(ui_tx) = ui_event_tx {
            let db = db.clone();
            let tx = ui_tx.clone();
            let agent_name = agent_name.to_string();
            tokio::spawn(async move {
                crate::gateway::notify(
                    &db,
                    &tx,
                    "context_compaction",
                    &format!("Context compacted: {}", agent_name),
                    &format!(
                        "Agent {} session was force-compacted after a context overflow",
                        agent_name
                    ),
                    serde_json::json!({"agent": agent_name, "forced": true}),
                )
                .await
                .ok();
            });
        }
    }
}

/// Compact a specific session's messages via API.
/// Returns `(facts_extracted, new_message_count)`.
#[allow(clippy::too_many_arguments)]
pub async fn compact_session<F, Fut>(
    db: &sqlx::PgPool,
    provider: &dyn LlmProvider,
    compaction_provider: Option<&dyn LlmProvider>,
    language: &str,
    agent_name: &str,
    session_id: Uuid,
    _audit_queue: &crate::db::audit_queue::AuditQueue,
    index_facts: F,
) -> Result<(usize, usize)>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let rows = SessionManager::new(db.clone())
        .load_messages(session_id, Some(2000))
        .await?;
    if rows.len() < 4 {
        anyhow::bail!("session too short to compact ({} messages)", rows.len());
    }

    let mut messages: Vec<Message> = rows.iter().map(row_to_message).collect();

    // Force compaction by using max_tokens=1
    let facts = history::compact_if_needed(
        &mut messages,
        provider,
        compaction_provider,
        1,
        2,
        Some(language),
    )
    .await?;

    let facts_count = facts.as_ref().map(|f| f.len()).unwrap_or(0);

    if let Some(facts) = facts {
        index_facts(facts).await;
    }

    // Replace messages in DB (atomic transaction)
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM messages WHERE session_id = $1")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

    for msg in &messages {
        let role = match msg.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        };
        let tc_json = msg
            .tool_calls
            .as_ref()
            .and_then(|tc| serde_json::to_value(tc).ok());
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, agent_id) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(session_id)
        .bind(role)
        .bind(&msg.content)
        .bind(tc_json.as_ref())
        .bind(msg.tool_call_id.as_ref().map(|id| id.as_str()))
        .bind(if role == "assistant" {
            Some(agent_name)
        } else {
            None::<&str>
        })
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let new_count = messages.len();
    crate::db::audit::audit_spawn(
        db.clone(),
        agent_name.to_string(),
        crate::db::audit::event_types::COMPACTION,
        Some("api".to_string()),
        serde_json::json!({
            "session_id": session_id.to_string(),
            "facts": facts_count,
            "new_messages": new_count,
            "original_messages": rows.len(),
        }),
    );

    tracing::info!(
        session_id = %session_id, facts = facts_count,
        old = rows.len(), new = new_count, "session compacted via API"
    );

    Ok((facts_count, new_count))
}

#[cfg(test)]
mod tests {
    use super::truncate_preview;

    // Bug 17: emoji must not be split at a byte boundary.
    #[test]
    fn truncate_preview_preserves_emoji() {
        // "hello🔥world" — 11 Unicode scalars; the emoji is 4 bytes.
        // The old floor_char_boundary(10) could land at byte 9 (inside the
        // emoji), producing garbled output or a panic.  The new impl counts
        // chars, so max=10 takes 7 chars then appends "..." — the emoji at
        // position 5 is included in the prefix and is not split.
        let s = "hello🔥world";
        // 11 chars total; max=10 → prefix of 7 chars = "hello🔥w"
        assert_eq!(truncate_preview(s, 10), "hello🔥w...");
        // max=6 → prefix of 3 chars = "hel"
        assert_eq!(truncate_preview(s, 6), "hel...");
        // max=7 → prefix of 4 chars = "hell"; emoji is not included (char 5)
        assert_eq!(truncate_preview(s, 7), "hell...");
        // max=8 → prefix of 5 chars = "hello"; emoji next at position 5 is not split
        assert_eq!(truncate_preview(s, 8), "hello...");
        // max=9 → prefix of 6 chars = "hello🔥"; emoji is fully included
        assert_eq!(truncate_preview(s, 9), "hello🔥...");
    }

    // Bug 17: when max < 4 no ellipsis, just a bare prefix.
    #[test]
    fn truncate_preview_max_less_than_4_no_ellipsis() {
        let s = "abcdefgh";
        assert_eq!(truncate_preview(s, 3), "abc");
        assert_eq!(truncate_preview(s, 0), "");
        assert_eq!(truncate_preview(s, 1), "a");
    }

    #[test]
    fn truncate_preview_short_string_unchanged() {
        assert_eq!(truncate_preview("hi", 8), "hi");
    }

    #[test]
    fn truncate_preview_exact_length_unchanged() {
        assert_eq!(truncate_preview("hello", 5), "hello");
    }

    #[test]
    fn truncate_preview_ascii_truncation() {
        assert_eq!(truncate_preview("abcdefgh", 5), "ab...");
    }

    // Regression: truncate_tool_result must not panic when the byte offset
    // `len - 1500` lands mid-codepoint (multi-byte text like Russian ИТС
    // results). Byte 2000 below is the 2nd byte of the 2-byte 'я', which the
    // old raw slice `&result[len-1500..]` sliced through → panic → dead session.
    #[test]
    fn truncate_tool_result_no_panic_on_multibyte_boundary() {
        use super::truncate_tool_result;
        let result = format!("{}я{}", "a".repeat(1999), "b".repeat(1499));
        assert_eq!(result.len(), 3500); // 'я' occupies bytes 1999..=2000
        assert!(!result.is_char_boundary(2000)); // len-1500 is mid-codepoint
        // Huge context → remaining budget 0 → limit floors to 2000 < 3500,
        // so the function slices (and must not panic).
        let out = truncate_tool_result("gpt-4", &result, 100_000_000);
        assert!(out.contains("truncated"));
    }
}
