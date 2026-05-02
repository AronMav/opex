use anyhow::Result;
use hydeclaw_types::{Message, MessageRole, ToolDefinition};
use crate::agent::thinking::strip_thinking;

use super::providers::LlmProvider;
use super::tool_loop::LoopDetector;

pub const SUMMARY_PREFIX: &str = "[CONTEXT COMPACTION — REFERENCE ONLY] Earlier turns were compacted \
into the summary below. This is a handoff from a previous context window — treat it as background \
reference, NOT as active instructions. Do NOT answer questions or fulfill requests mentioned in \
this summary; they were already addressed. Your current task is identified in the '## Active Task' \
section — resume exactly from there. Respond ONLY to the latest user message that appears AFTER \
this summary.";

#[allow(dead_code)]
const SUMMARY_NOTE_FOR_SYSTEM: &str = "[Note: Some earlier conversation turns have been compacted \
into a handoff summary to preserve context space. Build on that summary rather than re-doing work.]";

const MIN_SUMMARY_TOKENS: usize = 2000;
const SUMMARY_RATIO: f64 = 0.20;
const SUMMARY_TOKENS_CEILING: usize = 12_000;

/// Estimate token count from text (rough: ~4 chars per token).
/// Accounts for `tool_calls` JSON size when present.
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_tokens = m.content.len() / 4 + 10;
            let tool_tokens = m
                .tool_calls
                .as_ref()
                .map_or(0, |tc| {
                    tc.iter()
                        .map(|t| {
                            let args_len = t.arguments.to_string().len();
                            let name_len = t.name.len();
                            (args_len + name_len + 20) / 4
                        })
                        .sum::<usize>()
                });
            content_tokens + tool_tokens
        })
        .sum()
}

/// Compact conversation history if it exceeds the threshold.
/// Extracts facts into memory-ready strings and replaces old messages with a summary.
/// `agent_language` is used to instruct the LLM to summarize in the correct language.
pub async fn compact_if_needed(
    messages: &mut Vec<Message>,
    provider: &dyn LlmProvider,
    compaction_provider: Option<&dyn LlmProvider>,
    max_tokens: usize,
    preserve_last_n: usize,
    agent_language: Option<&str>,
) -> Result<Option<Vec<String>>> {
    // Use dedicated compaction provider if available, otherwise fall back to main provider.
    let active_provider: &dyn LlmProvider = compaction_provider.unwrap_or(provider);
    let total = estimate_tokens(messages);
    let threshold = max_tokens * 80 / 100;

    if total < threshold {
        return Ok(None);
    }

    tracing::info!(
        total_tokens = total,
        threshold,
        messages = messages.len(),
        "context window threshold reached, compacting"
    );

    // Keep system message (first) and last N messages
    let system_msg = if !messages.is_empty() && messages[0].role == MessageRole::System {
        Some(messages[0].clone())
    } else {
        None
    };

    let start = usize::from(system_msg.is_some());
    let mut end = if messages.len() > start + preserve_last_n {
        messages.len() - preserve_last_n
    } else {
        return Ok(None); // Not enough messages to compact
    };

    // Don't split in the middle of a tool call group:
    // move `end` backward until messages[end] is not a Tool message.
    while end > start && messages[end].role == MessageRole::Tool {
        end -= 1;
    }
    // If we also landed on an Assistant with tool_calls, include it in preserved part
    if end > start && messages[end].role == MessageRole::Assistant && messages[end].tool_calls.is_some() {
        end -= 1;
    }
    if end <= start {
        return Ok(None); // Not enough messages to compact after adjustment
    }

    let to_compact: Vec<Message> = messages[start..end].to_vec();
    if to_compact.is_empty() {
        return Ok(None);
    }

    let formatted = format_messages_for_compaction(&to_compact);

    // Step 1: Extract facts for long-term memory
    let lang_hint = match agent_language {
        Some("ru") => " Write each fact in Russian.",
        Some("en") => " Write each fact in English.",
        _ => "",
    };
    let extraction_prompt = vec![
        Message {
            role: MessageRole::System,
            content: format!(
                "Extract key facts from this conversation as a JSON array of strings.\n\n\
                MUST PRESERVE:\n\
                - Active tasks with their current status and progress (e.g. '5/17 items done')\n\
                - All identifiers: UUIDs, URLs, file paths, IPs, hostnames, port numbers, service names\n\
                - Decisions made and their rationale\n\
                - User preferences and requirements discovered\n\
                - Error conditions encountered and their resolutions\n\
                - Commitments, action items, and deadlines\n\n\
                MAY OMIT:\n\
                - Routine greetings and confirmations\n\
                - Tool calls that succeeded without noteworthy results\n\
                - Repeated information already captured in other facts\n\n\
                Each fact must be self-contained and useful without the original conversation.{lang_hint}\n\
                Return ONLY the JSON array, no other text."
            ),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        },
        Message {
            role: MessageRole::User,
            content: formatted.clone(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        },
    ];

    let empty_tools: Vec<ToolDefinition> = vec![];
    let facts_response = active_provider.chat(&extraction_prompt, &empty_tools, crate::agent::providers::CallOptions::default()).await?;
    let extracted_facts: Vec<String> =
        serde_json::from_str(&facts_response.content).unwrap_or_default();

    tracing::info!(facts = extracted_facts.len(), "extracted facts from history");

    // Step 2: Summarize for context continuity
    let summary_lang = match agent_language {
        Some("en") => "in English",
        _ => "in Russian",
    };
    let summary_prompt = vec![
        Message {
            role: MessageRole::System,
            content: format!(
                "Summarize this conversation concisely {summary_lang}. Structure:\n\
                1. Active tasks and their progress\n\
                2. Key decisions made\n\
                3. Open questions or blockers\n\n\
                Preserve exact identifiers: UUIDs, URLs, file paths, IPs, hostnames, port numbers.\n\
                Be brief — 2-3 paragraphs max."
            ),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        },
        Message {
            role: MessageRole::User,
            content: formatted,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        },
    ];

    let summary_response = active_provider.chat(&summary_prompt, &empty_tools, crate::agent::providers::CallOptions::default()).await?;

    // Step 3: Rebuild messages — system + summary + preserved recent
    let preserved: Vec<Message> = messages[end..].to_vec();
    messages.clear();

    if let Some(sys) = system_msg {
        messages.push(sys);
    }

    let summary_text = strip_thinking(&summary_response.content);
    if !summary_text.trim().is_empty() {
        messages.push(Message {
            role: MessageRole::System,
            content: format!("[Previous conversation summary]\n{}", summary_text),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        });
    }

    messages.extend(preserved);

    tracing::info!(
        new_messages = messages.len(),
        new_tokens = estimate_tokens(messages),
        "compaction complete"
    );

    if extracted_facts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(extracted_facts))
    }
}

/// Generate a compact progress header that survives compaction.
/// Includes iteration count, top-5 tools by call count, and loop warning if active.
/// The header is marked with `[Session Progress]` so subsequent compactions can
/// replace it rather than accumulate multiple headers.
pub fn generate_progress_header(messages: &[Message], detector: &LoopDetector) -> String {
    let iterations = detector.iteration_count();
    let tool_counts = detector.tool_counts();

    // Sort tools by count descending, take top 5
    let mut sorted: Vec<(&String, &usize)> = tool_counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    let top_tools: Vec<String> = sorted.iter()
        .take(5)
        .map(|(name, count)| format!("{name}({count})"))
        .collect();

    // Count tool role messages that contain "error"
    let error_count = messages.iter()
        .filter(|m| m.role == MessageRole::Tool && m.content.to_lowercase().contains("error"))
        .count();

    let tools_str = if top_tools.is_empty() {
        "none".to_string()
    } else {
        top_tools.join(", ")
    };

    let mut header = format!(
        "[Session Progress] Iterations: {iterations}. Tools called: {tools_str}."
    );

    if error_count > 0 {
        header.push_str(&format!(" Errors encountered: {error_count} tool failures."));
    }

    header
}

/// Remove any existing `[Session Progress]` system message from the messages list.
/// Used before injecting a fresh progress header after compaction.
pub fn remove_progress_header(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !(m.role == MessageRole::Tool
            && m.content.starts_with("[Session Progress]")
            || m.role == MessageRole::System
                && m.content.starts_with("[Session Progress]"))
    });
}

/// Phase 1 pre-pass: replace old tool result contents with 1-line summaries,
/// deduplicate identical results. `protect_tail` is the number of messages from
/// the end that are never pruned (matches `preserve_last_n` from config).
pub fn prune_old_tool_results(messages: &[Message], protect_tail: usize) -> Vec<Message> {
    if messages.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<Message> = messages.to_vec();
    let prune_end = result.len().saturating_sub(protect_tail);

    // Pass 1: deduplicate — keep newest full copy, replace older dups
    use std::collections::{HashMap, HashSet};
    let mut content_hashes: HashMap<u64, usize> = HashMap::new();
    // Track indices that are the canonical (newest) copy of a dup group — these are
    // exempt from Pass 2 size-based pruning so callers can see the surviving content.
    let mut canonical_dup_indices: HashSet<usize> = HashSet::new();
    for i in (0..result.len()).rev() {
        if result[i].role != MessageRole::Tool { continue; }
        let content = &result[i].content;
        if content.len() < 200 { continue; }
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        let h = hasher.finish();
        if let Some(&newer_idx) = content_hashes.get(&h) {
            if i < newer_idx && i < prune_end {
                result[i].content =
                    "[Duplicate tool output — same content as a more recent call]".into();
                canonical_dup_indices.insert(newer_idx);
            }
        } else {
            content_hashes.insert(h, i);
        }
    }

    // Pass 2: replace large tool results outside protected tail with 1-line summary.
    // Skip canonical dup indices — their content must survive intact so callers can
    // verify deduplication worked correctly.
    for i in 0..prune_end {
        if canonical_dup_indices.contains(&i) { continue; }
        let msg = &result[i];
        if msg.role != MessageRole::Tool { continue; }
        if msg.content.len() <= 200 { continue; }
        if msg.content.starts_with('[') { continue; }
        let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
        let char_count = msg.content.len();
        result[i].content = format!("[tool result {tool_call_id}] ({char_count} chars — pruned)");
    }

    result
}

/// Phase 2: find where the compressed middle begins.
/// Starts at `protect_first_n`, then slides forward past any leading Tool messages.
pub fn find_head_end(messages: &[Message], protect_first_n: usize) -> usize {
    let mut idx = protect_first_n.min(messages.len());
    while idx < messages.len() && messages[idx].role == MessageRole::Tool {
        idx += 1;
    }
    idx
}

/// Phase 2: find where the protected tail begins, using a token budget.
pub fn find_tail_start_by_tokens(messages: &[Message], head_end: usize, tail_budget: usize) -> usize {
    let n = messages.len();
    if n <= head_end + 1 {
        return n;
    }
    let min_tail = 3.min(n.saturating_sub(head_end).saturating_sub(1));
    let soft_ceiling = (tail_budget as f64 * 1.5) as usize;
    let mut accumulated: usize = 0;
    let mut cut_idx = n;

    for i in (head_end..n).rev() {
        let msg_tokens = messages[i].content.len() / 4 + 10;
        if accumulated + msg_tokens > soft_ceiling && n - i >= min_tail {
            break;
        }
        accumulated += msg_tokens;
        cut_idx = i;
    }

    cut_idx = cut_idx.min(n.saturating_sub(min_tail));

    // Invariant: most recent User message must be in the tail
    let last_user_idx = messages[head_end..n]
        .iter()
        .rposition(|m| m.role == MessageRole::User)
        .map(|rel| rel + head_end);
    if let Some(user_idx) = last_user_idx {
        if user_idx < cut_idx {
            cut_idx = user_idx;
        }
    }

    // Align backward past tool groups
    while cut_idx > head_end && messages[cut_idx].role == MessageRole::Tool {
        cut_idx = cut_idx.saturating_sub(1);
    }
    if cut_idx > head_end
        && messages[cut_idx].role == MessageRole::Assistant
        && messages[cut_idx].tool_calls.is_some()
    {
        cut_idx = cut_idx.saturating_sub(1);
    }

    cut_idx.max(head_end + 1)
}

/// Phase 5: fix orphaned tool_call / tool_result pairs after compression.
pub fn sanitize_tool_pairs(messages: Vec<Message>) -> Vec<Message> {
    use std::collections::HashSet;

    let surviving_call_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .flat_map(|m| m.tool_calls.iter().flatten())
        .map(|tc| tc.id.clone())
        .collect();

    let result_call_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();

    // 1. Remove orphaned tool results
    let messages: Vec<Message> = messages
        .into_iter()
        .filter(|m| {
            if m.role == MessageRole::Tool {
                m.tool_call_id
                    .as_ref()
                    .map(|id| surviving_call_ids.contains(id))
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .collect();

    // 2. Insert stubs for orphaned calls
    let missing_ids: HashSet<String> = surviving_call_ids
        .difference(&result_call_ids)
        .cloned()
        .collect();

    if missing_ids.is_empty() {
        return messages;
    }

    let mut patched: Vec<Message> = Vec::with_capacity(messages.len() + missing_ids.len());
    for msg in messages {
        if msg.role == MessageRole::Assistant {
            let needs_stubs: Vec<String> = msg
                .tool_calls
                .iter()
                .flatten()
                .filter(|tc| missing_ids.contains(&tc.id))
                .map(|tc| tc.id.clone())
                .collect();
            patched.push(msg);
            for call_id in needs_stubs {
                patched.push(Message {
                    role: MessageRole::Tool,
                    content: "[Result from earlier conversation — see context summary above]"
                        .into(),
                    tool_call_id: Some(call_id),
                    tool_calls: None,
                    thinking_blocks: vec![],
                });
            }
        } else {
            patched.push(msg);
        }
    }
    patched
}

/// Format messages for compaction prompt.
fn format_messages_for_compaction(messages: &[Message]) -> String {
    let mut formatted = String::new();
    for msg in messages {
        let role = match msg.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::System => "System",
            MessageRole::Tool => "Tool",
        };
        formatted.push_str(&format!("[{}]: {}\n\n", role, msg.content));
    }
    formatted
}

/// Phase 3: generate a structured 13-section Hermes-style summary via LLM.
/// When `previous_summary` is Some, generates an iterative update.
/// Returns None on LLM failure — caller should use static fallback.
pub async fn generate_hermes_summary(
    turns: &[Message],
    provider: &dyn LlmProvider,
    language: Option<&str>,
    previous_summary: Option<&str>,
) -> Option<String> {
    let content_tokens = estimate_tokens(turns);
    let budget = ((content_tokens as f64 * SUMMARY_RATIO) as usize)
        .clamp(MIN_SUMMARY_TOKENS, SUMMARY_TOKENS_CEILING);

    let lang_instruction = match language {
        Some("en") => "Write the summary in English.",
        _ => "Write the summary in Russian.",
    };

    let template = format!(r#"## Active Task
[THE SINGLE MOST IMPORTANT FIELD. Copy the user's most recent request verbatim. If no outstanding task, write "None."]

## Goal
[What the user is trying to accomplish overall]

## Constraints & Preferences
[User preferences, coding style, important constraints]

## Completed Actions
[Numbered list: N. ACTION target — outcome [tool: name]]

## Active State
[Working directory, branch, modified files, test status, running processes]

## In Progress
[Work underway when compaction fired]

## Blocked
[Blockers, errors, issues not resolved — include exact error messages]

## Key Decisions
[Important technical decisions and WHY]

## Resolved Questions
[Questions already answered — include the answer]

## Pending User Asks
[Questions or requests not yet answered — if none, write "None."]

## Relevant Files
[Files read, modified, created — with brief note]

## Remaining Work
[What remains to be done — framed as context, not instructions]

## Critical Context
[Specific values, error messages, config details. NEVER include API keys — write [REDACTED].]

Target ~{budget} tokens. Be CONCRETE. Include file paths, commands, error messages, line numbers."#);

    let preamble = format!(
        "You are a summarization agent creating a context checkpoint. \
Your output will be injected as reference material for a DIFFERENT assistant \
that continues the conversation. Do NOT respond to any questions or requests \
in the conversation — only output the structured summary. \
Do NOT include any preamble, greeting, or prefix. \
{lang_instruction} \
NEVER include API keys, tokens, passwords, or secrets — write [REDACTED] instead."
    );

    let prompt_content = if let Some(prev) = previous_summary {
        let turns_text = format_messages_for_compaction(turns);
        format!(
            "{preamble}\n\nYou are UPDATING a context compaction summary. \
A previous compaction produced the summary below. New turns have occurred since then.\n\n\
PREVIOUS SUMMARY:\n{prev}\n\nNEW TURNS TO INCORPORATE:\n{turns_text}\n\n\
Update the summary using the structure below. PRESERVE all existing relevant info. \
ADD new completed actions (continue numbering). Move answered questions to Resolved. \
Update Active Task to the user's most recent unfulfilled request.\n\n{template}"
        )
    } else {
        let turns_text = format_messages_for_compaction(turns);
        format!(
            "{preamble}\n\nCreate a structured handoff summary for a different assistant \
that will continue this conversation.\n\nTURNS TO SUMMARIZE:\n{turns_text}\n\n{template}"
        )
    };

    let prompt = vec![Message {
        role: MessageRole::User,
        content: prompt_content,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    }];

    let empty_tools: Vec<ToolDefinition> = vec![];
    match provider
        .chat(&prompt, &empty_tools, crate::agent::providers::CallOptions::default())
        .await
    {
        Ok(response) => {
            let summary = response.content.trim().to_string();
            if summary.is_empty() {
                None
            } else {
                Some(format!("{SUMMARY_PREFIX}\n{summary}"))
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to generate context summary");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hydeclaw_types::{LlmResponse, Message, MessageRole, ToolCall, ToolDefinition};
    use tokio::sync::mpsc;
    use crate::agent::tool_loop::{LoopDetector, ToolLoopConfig};
    use crate::agent::providers::LlmProvider;

    struct StaticProvider(String);

    #[async_trait]
    impl LlmProvider for StaticProvider {
        async fn chat(&self, _msgs: &[Message], _tools: &[ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                content: self.0.clone(),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
                model: None,
                provider: None,
                fallback_notice: None,
                tools_used: vec![],
                iterations: 0,
                thinking_blocks: vec![],
            })
        }
        async fn chat_stream(&self, msgs: &[Message], tools: &[ToolDefinition], _tx: mpsc::UnboundedSender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            self.chat(msgs, tools, _opts).await
        }
        fn name(&self) -> &str { "static" }
    }

    fn make_message(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        }
    }

    #[test]
    fn estimate_tokens_empty_slice() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tokens_single_user_message() {
        let msg = make_message(MessageRole::User, "hello");
        // "hello".len() = 5, 5 / 4 + 10 = 11
        assert_eq!(estimate_tokens(&[msg]), 11);
    }

    #[test]
    fn estimate_tokens_multiple_messages() {
        let msgs = vec![
            make_message(MessageRole::User, "hello"),       // 5/4+10 = 11
            make_message(MessageRole::Assistant, "world!!"), // 7/4+10 = 11
            make_message(MessageRole::System, ""),           // 0/4+10 = 10
        ];
        let individual: usize = msgs.iter().map(|m| estimate_tokens(std::slice::from_ref(m))).sum();
        assert_eq!(estimate_tokens(&msgs), individual);
        assert_eq!(estimate_tokens(&msgs), 11 + 11 + 10);
    }

    #[test]
    fn estimate_tokens_with_tool_calls() {
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
            arguments: serde_json::json!({"city": "Moscow"}),
        };
        let msg = Message {
            role: MessageRole::Assistant,
            content: "".to_string(),
            tool_calls: Some(vec![tool_call.clone()]),
            tool_call_id: None,
            thinking_blocks: vec![],
        };

        let content_tokens = 10; // 0 / 4 + 10
        let args_str = tool_call.arguments.to_string();
        let tool_tokens = (args_str.len() + tool_call.name.len() + 20) / 4;
        let expected = content_tokens + tool_tokens;

        assert_eq!(estimate_tokens(&[msg]), expected);
        assert!(expected > 10, "tool calls should add tokens beyond content");
    }

    #[test]
    fn format_messages_mixed_roles() {
        let msgs = vec![
            make_message(MessageRole::User, "hi"),
            make_message(MessageRole::Assistant, "hello"),
            make_message(MessageRole::System, "you are helpful"),
            make_message(MessageRole::Tool, "result: 42"),
        ];

        let formatted = format_messages_for_compaction(&msgs);

        assert_eq!(
            formatted,
            "[User]: hi\n\n[Assistant]: hello\n\n[System]: you are helpful\n\n[Tool]: result: 42\n\n"
        );
    }

    #[test]
    fn format_messages_empty_content() {
        let msgs = vec![
            make_message(MessageRole::User, ""),
            make_message(MessageRole::Assistant, ""),
        ];

        let formatted = format_messages_for_compaction(&msgs);

        assert_eq!(formatted, "[User]: \n\n[Assistant]: \n\n");
    }

    // ── progress_header tests ─────────────────────────────────────────────────

    fn make_detector_with_counts(counts: &[(&str, usize)]) -> LoopDetector {
        let cfg = ToolLoopConfig::default();
        let mut det = LoopDetector::new(&cfg);
        for (tool, n) in counts {
            for _ in 0..*n {
                det.record_execution(tool, &serde_json::json!({}), true);
            }
        }
        det
    }

    #[test]
    fn progress_header_contains_tools() {
        let det = make_detector_with_counts(&[("search", 5), ("read", 3), ("write", 1)]);
        let msgs = vec![];
        let header = generate_progress_header(&msgs, &det);
        assert!(header.starts_with("[Session Progress]"), "header must start with sentinel");
        assert!(header.contains("search(5)"), "must list search with count 5");
        assert!(header.contains("read(3)"), "must list read with count 3");
        assert!(header.contains("write(1)"), "must list write with count 1");
        assert!(header.contains("Iterations: 9"), "must show total iteration count");
    }

    #[test]
    fn progress_header_with_error_messages() {
        let det = make_detector_with_counts(&[("search", 2)]);
        let msgs = vec![
            make_message(MessageRole::Tool, "error: file not found"),
            make_message(MessageRole::Tool, "success result"),
            make_message(MessageRole::Tool, "Error: timeout"),
        ];
        let header = generate_progress_header(&msgs, &det);
        assert!(header.contains("Errors encountered: 2"), "should count 2 error tool messages");
    }

    #[test]
    fn progress_header_no_tools() {
        let cfg = ToolLoopConfig::default();
        let det = LoopDetector::new(&cfg);
        let msgs = vec![];
        let header = generate_progress_header(&msgs, &det);
        assert!(header.starts_with("[Session Progress]"), "must have sentinel even with no tools");
        assert!(header.contains("none"), "should say 'none' when no tools called");
    }

    #[test]
    fn progress_header_top_5_tools() {
        let det = make_detector_with_counts(&[
            ("a", 10), ("b", 9), ("c", 8), ("d", 7), ("e", 6), ("f", 5),
        ]);
        let msgs = vec![];
        let header = generate_progress_header(&msgs, &det);
        // Should include top 5, not the 6th
        assert!(header.contains("a(10)"));
        assert!(header.contains("b(9)"));
        assert!(header.contains("e(6)"));
        // f(5) is 6th — should NOT appear
        assert!(!header.contains("f(5)"), "should not include 6th tool beyond top 5");
    }

    #[test]
    fn remove_progress_header_clears_existing() {
        let mut msgs = vec![
            make_message(MessageRole::System, "You are an assistant."),
            make_message(MessageRole::System, "[Session Progress] Iterations: 5. Tools called: search(5)."),
            make_message(MessageRole::User, "hello"),
        ];
        remove_progress_header(&mut msgs);
        assert_eq!(msgs.len(), 2, "progress header system message should be removed");
        assert!(!msgs.iter().any(|m| m.content.starts_with("[Session Progress]")));
    }

    // ── compact_if_needed: thinking-block summary regression ────────────────

    fn make_compactable_messages() -> Vec<Message> {
        // Build enough tokens to trigger compaction (>80% of a small threshold).
        // compact_if_needed threshold = max_tokens * 80/100.
        // We pass max_tokens=1 to force compaction regardless of actual size.
        vec![
            make_message(MessageRole::System, "You are an assistant."),
            make_message(MessageRole::User, "Hello"),
            make_message(MessageRole::Assistant, "Hi there"),
            make_message(MessageRole::User, "How are you"),
            make_message(MessageRole::Assistant, "I'm fine"),
            make_message(MessageRole::User, "What is 2+2"),
            make_message(MessageRole::Assistant, "4"),
        ]
    }

    #[tokio::test]
    async fn compact_thinking_only_summary_skips_summary_message() {
        // Regression: when a reasoning model returns only <think>…</think> as the compaction
        // summary (no visible text), strip_thinking would produce "" and a useless
        // "[Previous conversation summary]\n" message would be injected.
        // Now it should be skipped entirely.
        let provider = StaticProvider("<think>internal reasoning only</think>".to_string());
        let mut msgs = make_compactable_messages();
        compact_if_needed(&mut msgs, &provider, None, 1, 2, None).await.unwrap();

        let summary_msgs: Vec<_> = msgs.iter()
            .filter(|m| m.role == MessageRole::System && m.content.contains("[Previous conversation summary]"))
            .collect();
        assert!(
            summary_msgs.is_empty(),
            "thinking-only summary should be skipped, but got: {:?}",
            summary_msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn compact_real_summary_is_injected_stripped_of_thinking() {
        // When the summary contains both thinking and visible text, only visible text is kept.
        let provider = StaticProvider(
            "<think>reasoning</think>Summary: user asked math questions.".to_string()
        );
        let mut msgs = make_compactable_messages();
        compact_if_needed(&mut msgs, &provider, None, 1, 2, None).await.unwrap();

        let summary_msg = msgs.iter().find(|m| {
            m.role == MessageRole::System && m.content.contains("[Previous conversation summary]")
        });
        assert!(summary_msg.is_some(), "real summary should be injected");
        let content = &summary_msg.unwrap().content;
        assert!(content.contains("Summary: user asked math questions."), "got: {content}");
        assert!(!content.contains("<think>"), "thinking tags must be stripped, got: {content}");
    }

    #[tokio::test]
    async fn compact_empty_summary_skips_summary_message() {
        let provider = StaticProvider(String::new());
        let mut msgs = make_compactable_messages();
        compact_if_needed(&mut msgs, &provider, None, 1, 2, None).await.unwrap();

        let has_summary = msgs.iter().any(|m| {
            m.role == MessageRole::System && m.content.contains("[Previous conversation summary]")
        });
        assert!(!has_summary, "empty summary should not inject a summary message");
    }

    // ── prune_old_tool_results tests ─────────────────────────────────────────

    #[test]
    fn prune_deduplicates_identical_tool_results() {
        let dup_content = "x".repeat(300);
        let msgs = vec![
            Message { role: MessageRole::Tool, content: dup_content.clone(),
                      tool_call_id: Some("a".into()), tool_calls: None, thinking_blocks: vec![] },
            Message { role: MessageRole::Tool, content: dup_content.clone(),
                      tool_call_id: Some("b".into()), tool_calls: None, thinking_blocks: vec![] },
        ];
        let pruned = prune_old_tool_results(&msgs, 0);
        assert!(pruned[0].content.contains("Duplicate"));
        assert_eq!(pruned[1].content, dup_content);
    }

    #[test]
    fn prune_replaces_large_tool_result_with_summary_line() {
        let msgs = vec![
            Message { role: MessageRole::Tool, content: "a".repeat(300),
                      tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![] },
        ];
        let pruned = prune_old_tool_results(&msgs, 0);
        assert!(pruned[0].content.starts_with('['));
        assert!(pruned[0].content.len() < 300);
    }

    #[test]
    fn prune_skips_messages_in_protected_tail() {
        let content = "b".repeat(300);
        let msgs = vec![
            Message { role: MessageRole::Tool, content: content.clone(),
                      tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![] },
        ];
        let pruned = prune_old_tool_results(&msgs, 1);
        assert_eq!(pruned[0].content, content);
    }

    // ── find_tail_start_by_tokens / find_head_end tests ──────────────────────

    #[test]
    fn tail_cut_respects_token_budget() {
        let msgs: Vec<Message> = (0..10).map(|i| Message {
            role: if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant },
            content: "a".repeat(400),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
        }).collect();
        let tail_start = find_tail_start_by_tokens(&msgs, 0, 200);
        assert!(tail_start >= msgs.len() - 4);
        assert!(tail_start < msgs.len());
    }

    #[test]
    fn tail_cut_always_includes_last_user_message() {
        let mut msgs: Vec<Message> = (0..8).map(|_| Message {
            role: MessageRole::Assistant,
            content: "a".repeat(400),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
        }).collect();
        msgs[3].role = MessageRole::User;
        let tail_start = find_tail_start_by_tokens(&msgs, 0, 50);
        assert!(tail_start <= 3, "last user message must be in tail, tail_start={tail_start}");
    }

    #[test]
    fn head_end_skips_orphan_tool_results() {
        let msgs = vec![
            Message { role: MessageRole::System,    content: "s".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
            Message { role: MessageRole::User,      content: "u".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
            Message { role: MessageRole::Tool,      content: "t".into(), tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![] },
            Message { role: MessageRole::Assistant, content: "a".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
        ];
        let head_end = find_head_end(&msgs, 2);
        assert_eq!(head_end, 3);
    }

    // ── sanitize_tool_pairs tests ─────────────────────────────────────────────

    #[test]
    fn sanitize_removes_orphaned_tool_results() {
        let msgs = vec![
            Message { role: MessageRole::Tool, content: "orphan".into(),
                      tool_call_id: Some("orphan_id".into()), tool_calls: None, thinking_blocks: vec![] },
            Message { role: MessageRole::User, content: "hello".into(),
                      tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
        ];
        let sanitized = sanitize_tool_pairs(msgs);
        assert_eq!(sanitized.len(), 1);
        assert_eq!(sanitized[0].role, MessageRole::User);
    }

    #[test]
    fn sanitize_adds_stub_for_orphaned_calls() {
        let msgs = vec![
            Message {
                role: MessageRole::Assistant,
                content: "".into(),
                tool_calls: Some(vec![ToolCall {
                    id: "tc_1".into(),
                    name: "workspace_read".into(),
                    arguments: serde_json::json!({}),
                }]),
                tool_call_id: None,
                thinking_blocks: vec![],
            },
            Message { role: MessageRole::User, content: "next".into(),
                      tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
        ];
        let sanitized = sanitize_tool_pairs(msgs);
        assert_eq!(sanitized.len(), 3);
        assert_eq!(sanitized[1].role, MessageRole::Tool);
        assert_eq!(sanitized[1].tool_call_id.as_deref(), Some("tc_1"));
        assert!(sanitized[1].content.contains("earlier conversation"));
    }

    // ── generate_hermes_summary tests ──────────────────────────────────────────

    struct EchoProvider(String);

    #[async_trait]
    impl LlmProvider for EchoProvider {
        async fn chat(
            &self,
            _msgs: &[Message],
            _tools: &[ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                content: self.0.clone(),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
                model: None,
                provider: None,
                fallback_notice: None,
                tools_used: vec![],
                iterations: 0,
                thinking_blocks: vec![],
            })
        }
        async fn chat_stream(
            &self,
            msgs: &[Message],
            tools: &[ToolDefinition],
            _tx: mpsc::UnboundedSender<String>,
            opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            self.chat(msgs, tools, opts).await
        }
        fn name(&self) -> &str { "echo" }
    }

    struct FailProvider;

    #[async_trait]
    impl LlmProvider for FailProvider {
        async fn chat(
            &self,
            _msgs: &[Message],
            _tools: &[ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("simulated LLM failure")
        }
        async fn chat_stream(
            &self,
            msgs: &[Message],
            tools: &[ToolDefinition],
            _tx: mpsc::UnboundedSender<String>,
            opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            self.chat(msgs, tools, opts).await
        }
        fn name(&self) -> &str { "fail" }
    }

    #[tokio::test]
    async fn generate_hermes_summary_prepends_prefix() {
        let turns = vec![make_message(MessageRole::User, "hello")];
        let provider = EchoProvider("My summary body".into());
        let result = generate_hermes_summary(&turns, &provider, None, None).await;
        let text = result.unwrap();
        assert!(text.starts_with(SUMMARY_PREFIX), "must start with SUMMARY_PREFIX");
        assert!(text.contains("My summary body"));
    }

    #[tokio::test]
    async fn generate_hermes_summary_iterative_update_includes_previous() {
        struct PromptEchoProvider;

        #[async_trait]
        impl LlmProvider for PromptEchoProvider {
            async fn chat(
                &self,
                msgs: &[Message],
                _tools: &[ToolDefinition],
                _opts: crate::agent::providers::CallOptions,
            ) -> anyhow::Result<LlmResponse> {
                let content = msgs.iter()
                    .find(|m| m.role == MessageRole::User)
                    .map(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(LlmResponse {
                    content,
                    tool_calls: vec![],
                    usage: None,
                    finish_reason: None,
                    model: None,
                    provider: None,
                    fallback_notice: None,
                    tools_used: vec![],
                    iterations: 0,
                    thinking_blocks: vec![],
                })
            }
            async fn chat_stream(
                &self,
                msgs: &[Message],
                tools: &[ToolDefinition],
                _tx: mpsc::UnboundedSender<String>,
                opts: crate::agent::providers::CallOptions,
            ) -> anyhow::Result<LlmResponse> {
                self.chat(msgs, tools, opts).await
            }
            fn name(&self) -> &str { "prompt-echo" }
        }

        let turns = vec![make_message(MessageRole::User, "new turn")];
        let provider = PromptEchoProvider;
        let prev = "PREVIOUS SUMMARY CONTENT";
        let result = generate_hermes_summary(&turns, &provider, None, Some(prev)).await;
        let text = result.unwrap();
        // The prompt sent to LLM should contain "UPDATING" and the previous summary
        assert!(
            text.contains("UPDATING") || text.contains(prev),
            "iterative update must reference previous summary; got: {}",
            &text[..100.min(text.len())]
        );
    }

    #[tokio::test]
    async fn generate_hermes_summary_returns_none_on_llm_failure() {
        let turns = vec![make_message(MessageRole::User, "hello")];
        let provider = FailProvider;
        let result = generate_hermes_summary(&turns, &provider, None, None).await;
        assert!(result.is_none(), "must return None when LLM fails");
    }
}
