//! Thinking block handling — stripping `<think>`, `<thinking>`, `<thought>`, `<antthinking>`
//! tags from LLM responses and streaming chunks.

use hydeclaw_types::{IncomingMessage, Message, MessageRole};

/// Check if thinking blocks should be stripped for this message.
/// Returns false (don't strip) when /think directive is present in context.
#[allow(dead_code)]
pub(crate) fn should_strip_thinking(msg: &IncomingMessage) -> bool {
    !msg.context
        .get("directives")
        .and_then(|d| d.get("think"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Conditionally strip thinking blocks based on message directives and engine thinking level.
/// Level 0: always strip. Level 1-2: strip (summary TBD). Level 3+: preserve.
#[allow(dead_code)]
pub(crate) fn maybe_strip_thinking(text: &str, msg: &IncomingMessage, thinking_level: u8) -> String {
    // Per-message /think directive overrides engine level
    if !should_strip_thinking(msg) {
        return text.to_string();
    }
    if thinking_level >= 3 {
        // Level 3+ — preserve thinking blocks
        text.to_string()
    } else {
        strip_thinking(text)
    }
}

/// Remove `<think>...</think>` (and `<thinking>`, `<thought>`, `<antthinking>`) blocks
/// from LLM response. Uses ASCII case-insensitive search to avoid byte offset mismatches.
pub(crate) fn strip_thinking(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    loop {
        match find_open_tag_ascii(rest) {
            None => {
                result.push_str(rest);
                break;
            }
            Some(start) => {
                result.push_str(&rest[..start]);
                // Find end of opening tag '>'
                match rest[start..].find('>') {
                    Some(p) => {
                        let after_open = start + p + 1;
                        // Find closing tag
                        match find_close_tag_ascii(&rest[after_open..]) {
                            Some(close_len) => {
                                rest = &rest[after_open + close_len..];
                            }
                            None => break, // unclosed — strip rest
                        }
                    }
                    None => break, // unclosed tag — strip rest
                }
            }
        }
    }

    result.trim().to_string()
}

/// Stateful filter for stripping `<think>` blocks from streaming chunks.
/// Uses ASCII case-insensitive search — all tags are ASCII, safe on any UTF-8 text.
pub struct ThinkingFilter {
    in_thinking: bool,
    buffer: String,
}

impl ThinkingFilter {
    pub fn new() -> Self {
        Self {
            in_thinking: false,
            buffer: String::new(),
        }
    }

    /// Process a chunk, return text to emit (may be empty if inside thinking block).
    pub fn process(&mut self, chunk: &str) -> String {
        self.buffer.push_str(chunk);
        let mut output = String::new();

        loop {
            if self.in_thinking {
                if let Some(pos) = find_close_tag_ascii(&self.buffer) {
                    self.buffer = self.buffer[pos..].to_string();
                    self.in_thinking = false;
                    continue;
                }
                // Keep last 20 bytes (on char boundary) for partial close tag
                if self.buffer.len() > 20 {
                    let cut = self.buffer.floor_char_boundary(self.buffer.len() - 20);
                    self.buffer = self.buffer[cut..].to_string();
                }
                break;
            } else {
                if let Some(pos) = find_open_tag_ascii(&self.buffer) {
                    output.push_str(&self.buffer[..pos]);
                    self.buffer = self.buffer[pos..].to_string();
                    // Skip past the opening tag '>'
                    if let Some(end) = self.buffer.find('>') {
                        self.buffer = self.buffer[end + 1..].to_string();
                    } else {
                        self.buffer.clear();
                    }
                    self.in_thinking = true;
                    continue;
                }
                // Emit everything except potential partial tag at end
                let safe = safe_emit_len(&self.buffer);
                if safe > 0 {
                    output.push_str(&self.buffer[..safe]);
                    self.buffer = self.buffer[safe..].to_string();
                }
                break;
            }
        }

        output
    }
}

/// Find opening thinking tag using ASCII case-insensitive byte search.
/// Returns byte position of `<` — always a valid char boundary since `<` is ASCII.
pub(crate) fn find_open_tag_ascii(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    for tag in [
        b"<think>" as &[u8], b"<thinking>", b"<thinking ", b"<think ",
        b"<thought>", b"<antthinking>",
    ] {
        if let Some(pos) = bytes.windows(tag.len()).position(|w| w.eq_ignore_ascii_case(tag)) {
            return Some(pos);
        }
    }
    None
}

/// Find closing thinking tag, return byte position AFTER it.
pub(crate) fn find_close_tag_ascii(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    for tag in [
        b"</think>" as &[u8], b"</thinking>", b"</thought>", b"</antthinking>",
    ] {
        if let Some(pos) = bytes.windows(tag.len()).position(|w| w.eq_ignore_ascii_case(tag)) {
            return Some(pos + tag.len());
        }
    }
    None
}

/// How many bytes from the start are safe to emit (no partial `<` that could be a tag).
pub(crate) fn safe_emit_len(text: &str) -> usize {
    // `<` is ASCII (1 byte), so rfind always returns a valid char boundary
    match text.rfind('<') {
        Some(pos) => {
            let tail = &text.as_bytes()[pos..];
            let tail_lower: Vec<u8> = tail.iter().map(u8::to_ascii_lowercase).collect();
            // Check if tail could be a partial start of any thinking tag
            let could_be_tag = [
                b"<think>" as &[u8],
                b"<thinking>",
                b"<thought>",
                b"<antthinking>",
                b"</think>",
                b"</thinking>",
                b"</thought>",
                b"</antthinking>",
            ]
            .iter()
            .any(|tag| tag.starts_with(&tail_lower));
            if could_be_tag { pos } else { text.len() }
        }
        None => text.len(),
    }
}

/// Check if LLM response describes remaining work without executing it.
/// Used for auto-continue: nudge LLM to finish the task with tools.
pub(crate) fn looks_incomplete(text: &str) -> bool {
    let lower = text.to_lowercase();
    let trimmed = text.trim();

    // Pattern 1: Explicit "next steps" markers
    let markers = [
        "далее нужно", "следующий шаг", "осталось сделать", "затем нужно",
        "теперь нужно", "нужно ещё", "нужно еще", "приступаю к", "перехожу к",
        "next step", "remaining steps", "i'll now", "i will then", "starting with",
        "todo:", "need to also", "let's begin by",
    ];

    // Pattern 2: Structural incompleteness (open code blocks or dangling list markers)
    let is_open_code_block = trimmed.contains("```") && !trimmed.ends_with("```") && !trimmed.ends_with("```\n");
    let ends_with_list_marker = trimmed.ends_with(':') || trimmed.ends_with('-') || (!trimmed.is_empty() && trimmed.chars().last().unwrap().is_ascii_digit());

    let exclusions = [
        "можешь", "хочешь", "хотите", "если нужно", "при необходимости",
        "можно также", "рекомендую", "предлагаю", "жду твоих", "жду ваших",
        "would you like", "do you want", "if you need", "you can", "let me know",
    ];

    let has_marker = markers.iter().any(|m| lower.contains(m));
    let has_exclusion = exclusions.iter().any(|m| lower.contains(m));

    (has_marker || is_open_code_block || ends_with_list_marker) && !has_exclusion
}

/// Extract a non-empty result string from LLM `content` with a fallback to the last tool
/// message in `messages`.
///
/// When a reasoning model outputs only `<think>…</think>` with no visible text, `strip_thinking`
/// returns `""`. Rather than propagating an empty result, we fall back to the last tool-call
/// output (which contains the agent's actual work). If there are no tool messages either, we
/// return `"(no result)"`.
pub(crate) fn extract_result_text(content: &str, messages: &[Message]) -> String {
    let stripped = strip_thinking(content);
    if !stripped.trim().is_empty() {
        return stripped;
    }
    if let Some(last_tool) = messages.iter().rev().find(|m| m.role == MessageRole::Tool) {
        let preview: String = last_tool.content.chars().take(3000).collect();
        return format!("[No summary from agent. Last tool output:]\n{}", preview);
    }
    "(no result)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── find_open_tag_ascii ──────────────────────────────────────────────────

    #[test]
    fn open_tag_think_at_start() {
        assert_eq!(find_open_tag_ascii("<think>"), Some(0));
    }

    #[test]
    fn open_tag_thinking_at_start() {
        assert_eq!(find_open_tag_ascii("<thinking>"), Some(0));
    }

    #[test]
    fn open_tag_case_insensitive_upper() {
        assert_eq!(find_open_tag_ascii("<THINK>"), Some(0));
    }

    #[test]
    fn open_tag_antthinking_case_insensitive() {
        assert_eq!(find_open_tag_ascii("<antThinking>"), Some(0));
    }

    #[test]
    fn open_tag_text_before_tag() {
        assert_eq!(find_open_tag_ascii("hello <think>"), Some(6));
    }

    #[test]
    fn open_tag_no_tag_returns_none() {
        assert_eq!(find_open_tag_ascii("hello world"), None);
    }

    #[test]
    fn open_tag_partial_not_matched() {
        assert_eq!(find_open_tag_ascii("<thi"), None);
    }

    #[test]
    fn open_tag_thought() {
        assert_eq!(find_open_tag_ascii("<thought>"), Some(0));
    }

    #[test]
    fn open_tag_think_with_space() {
        assert_eq!(find_open_tag_ascii("<think hidden>"), Some(0));
    }

    // ── find_close_tag_ascii ─────────────────────────────────────────────────

    #[test]
    fn close_tag_think_returns_after_tag() {
        assert_eq!(find_close_tag_ascii("</think>"), Some(8));
    }

    #[test]
    fn close_tag_thinking_returns_after_tag() {
        assert_eq!(find_close_tag_ascii("</thinking>"), Some(11));
    }

    #[test]
    fn close_tag_case_insensitive() {
        assert_eq!(find_close_tag_ascii("</THINK>"), Some(8));
    }

    #[test]
    fn close_tag_no_tag_returns_none() {
        assert_eq!(find_close_tag_ascii("no close tag here"), None);
    }

    #[test]
    fn close_tag_text_before_tag() {
        assert_eq!(find_close_tag_ascii("stuff</think>more"), Some(13));
    }

    #[test]
    fn close_tag_thought() {
        assert_eq!(find_close_tag_ascii("</thought>"), Some(10));
    }

    #[test]
    fn close_tag_antthinking() {
        assert_eq!(find_close_tag_ascii("</antthinking>"), Some(14));
    }

    // ── safe_emit_len ────────────────────────────────────────────────────────

    #[test]
    fn safe_emit_no_angle_bracket_full_length() {
        let s = "hello world";
        assert_eq!(safe_emit_len(s), s.len());
    }

    #[test]
    fn safe_emit_text_ending_with_angle_bracket() {
        let s = "hello<";
        assert_eq!(safe_emit_len(s), 5);
    }

    #[test]
    fn safe_emit_complete_non_thinking_tag_full_length() {
        let s = "hello <b> world";
        assert_eq!(safe_emit_len(s), s.len());
    }

    #[test]
    fn safe_emit_partial_thinking_tag_at_end_truncated() {
        let s = "hello <thin";
        assert_eq!(safe_emit_len(s), 6);
    }

    #[test]
    fn safe_emit_empty_string() {
        assert_eq!(safe_emit_len(""), 0);
    }

    // ── strip_thinking ───────────────────────────────────────────────────────

    #[test]
    fn strip_no_tags_unchanged() {
        assert_eq!(strip_thinking("hello world"), "hello world");
    }

    #[test]
    fn strip_simple_think_block() {
        assert_eq!(
            strip_thinking("before<think>hidden</think>after"),
            "beforeafter"
        );
    }

    #[test]
    fn strip_thinking_tag() {
        assert_eq!(
            strip_thinking("a<thinking>x</thinking>b"),
            "ab"
        );
    }

    #[test]
    fn strip_case_insensitive() {
        assert_eq!(strip_thinking("<THINK>x</THINK>y"), "y");
    }

    #[test]
    fn strip_unclosed_tag_strips_rest() {
        assert_eq!(strip_thinking("before<think>rest"), "before");
    }

    #[test]
    fn strip_multiple_blocks() {
        assert_eq!(
            strip_thinking("a<think>1</think>b<think>2</think>c"),
            "abc"
        );
    }

    #[test]
    fn strip_empty_think_block() {
        assert_eq!(strip_thinking("<think></think>"), "");
    }

    #[test]
    fn strip_plain_text_preserved() {
        assert_eq!(strip_thinking("just text"), "just text");
    }

    // ── ThinkingFilter (streaming) ───────────────────────────────────────────

    #[test]
    fn filter_no_thinking_passthrough() {
        let mut f = ThinkingFilter::new();
        assert_eq!(f.process("hello"), "hello");
    }

    #[test]
    fn filter_complete_block_in_one_chunk() {
        let mut f = ThinkingFilter::new();
        assert_eq!(f.process("<think>hidden</think>visible"), "visible");
    }

    #[test]
    fn filter_split_open_tag_across_chunks() {
        let mut f = ThinkingFilter::new();
        let out1 = f.process("<thi");
        assert_eq!(out1, "", "partial open tag should be buffered, not emitted");
        let out2 = f.process("nk>hidden</think>after");
        assert_eq!(out2, "after");
    }

    #[test]
    fn filter_text_before_open_tag_emitted_immediately() {
        let mut f = ThinkingFilter::new();
        let out1 = f.process("before<think>");
        assert_eq!(out1, "before");
        let out2 = f.process("stuff</think>after");
        assert_eq!(out2, "after");
    }

    #[test]
    fn filter_multiple_chunks_no_thinking() {
        let mut f = ThinkingFilter::new();
        let out1 = f.process("hel");
        let out2 = f.process("lo");
        assert_eq!(out1 + &out2, "hello");
    }

    // ── extract_result_text ──────────────────────────────────────────────────

    fn tool_msg(content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: Some("tc1".to_string()),
            thinking_blocks: vec![],
        }
    }

    fn user_msg(content: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        }
    }

    #[test]
    fn extract_normal_text_returned_unchanged() {
        assert_eq!(extract_result_text("Hello world", &[]), "Hello world");
    }

    #[test]
    fn extract_strips_thinking_returns_visible_text() {
        let result = extract_result_text("<think>reasoning</think>Here is my answer.", &[]);
        assert_eq!(result, "Here is my answer.");
    }

    #[test]
    fn extract_only_thinking_falls_back_to_last_tool_message() {
        let msgs = vec![tool_msg("docker pull output: sha256:abc123")];
        let result = extract_result_text("<think>only thinking here</think>", &msgs);
        assert!(
            result.starts_with("[No summary from agent. Last tool output:]"),
            "expected fallback prefix, got: {result}"
        );
        assert!(result.contains("docker pull output: sha256:abc123"), "got: {result}");
    }

    #[test]
    fn extract_empty_content_no_tool_messages_returns_no_result() {
        assert_eq!(extract_result_text("", &[]), "(no result)");
    }

    #[test]
    fn extract_empty_content_uses_last_tool_message() {
        let msgs = vec![user_msg("do something"), tool_msg("search results here")];
        let result = extract_result_text("", &msgs);
        assert!(result.contains("search results here"), "got: {result}");
    }

    #[test]
    fn extract_uses_last_of_multiple_tool_messages() {
        let msgs = vec![tool_msg("first tool"), tool_msg("second tool")];
        let result = extract_result_text("", &msgs);
        assert!(result.contains("second tool"), "should use last tool msg, got: {result}");
        assert!(!result.contains("first tool"), "should not use first tool msg, got: {result}");
    }

    #[test]
    fn extract_tool_output_truncated_at_3000_chars() {
        let long_output = "x".repeat(5000);
        let msgs = vec![tool_msg(&long_output)];
        let result = extract_result_text("", &msgs);
        let preview_part = result.splitn(2, '\n').nth(1).unwrap_or("");
        assert_eq!(
            preview_part.chars().count(),
            3000,
            "preview should be capped at 3000 chars"
        );
    }

    #[test]
    fn extract_whitespace_only_content_treated_as_empty() {
        let msgs = vec![tool_msg("fallback content")];
        let result = extract_result_text("   \n\t  ", &msgs);
        assert!(result.contains("fallback content"), "whitespace-only should fall back, got: {result}");
    }

    #[test]
    fn extract_non_tool_messages_not_used_as_fallback() {
        let msgs = vec![user_msg("user says something")];
        let result = extract_result_text("", &msgs);
        assert_eq!(result, "(no result)", "only tool messages should be used as fallback");
    }
}
