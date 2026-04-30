use anyhow::Result;
use hydeclaw_types::{Message, MessageRole, ToolDefinition};
use crate::agent::thinking::strip_thinking;

use super::providers::LlmProvider;
use super::tool_loop::LoopDetector;

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
}
