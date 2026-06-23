//! Anthropic SSE streaming: event-typed delta accumulation.
//!
//! Owns the state machine that turns `content_block_start` /
//! `content_block_delta` / `content_block_stop` events into a
//! streaming `LlmResponse`. Aggregates token usage across
//! `message_start` and `message_delta` events.

/// Buffer for streaming usage data accumulated from SSE events.
/// `message_start` populates input/cache_*. `message_delta` updates output.
#[derive(Default)]
pub(super) struct StreamingAnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    /// True if any usage event (`message_start` or `message_delta`) was observed.
    /// If false at end of stream, usage stays None (no synthesized zeros).
    seen: bool,
}

impl StreamingAnthropicUsage {
    pub(super) fn into_token_usage(self) -> Option<opex_types::TokenUsage> {
        if !self.seen {
            return None;
        }
        // Optional cache hit log (mirrors non-streaming path).
        if let Some(cache_read) = self.cache_read_input_tokens
            && cache_read > 0
        {
            tracing::info!(
                cache_read,
                cache_create = self.cache_creation_input_tokens,
                "anthropic streaming cache hit"
            );
        }
        Some(opex_types::TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_input_tokens,
            cache_creation_tokens: self.cache_creation_input_tokens,
            reasoning_tokens: None,
        })
    }
}

/// Bundles the per-stream thinking-block state so `process_sse_event` doesn't
/// take five `&mut` accumulator parameters. Lives on the stack of the
/// streaming consume loop; reset on each new HTTP response.
#[derive(Default)]
pub(super) struct ThinkingState {
    /// Accumulated text inside the current `<thinking>` block (between
    /// `content_block_start` and `content_block_stop`).
    content: String,
    /// Accumulated cryptographic signature for the current thinking block.
    signature: String,
    /// True when we are between a thinking-block start and its stop.
    in_block: bool,
    /// Sealed `ThinkingBlock` records, one per closed thinking block.
    pub(super) blocks: Vec<opex_types::ThinkingBlock>,
}

/// Process one parsed Anthropic SSE event. Calls `emit_thinking` for thinking content
/// (open/close tags + deltas) and `emit_text` for text_delta content. Captures usage
/// from `message_start` (input + cache_*) and `message_delta` (cumulative output) into
/// `usage_buffer`.
pub(super) fn process_sse_event(
    event: &serde_json::Value,
    thinking: &mut ThinkingState,
    usage_buffer: &mut StreamingAnthropicUsage,
    mut emit_thinking: impl FnMut(String),
    mut emit_text: impl FnMut(String),
) {
    match event.get("type").and_then(|t| t.as_str()) {
        Some("message_start") => {
            if let Some(usage) = event.get("message").and_then(|m| m.get("usage")) {
                usage_buffer.seen = true;
                if let Some(n) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.input_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.output_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_creation_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
                if let Some(n) = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_read_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
            }
        }
        Some("message_delta") => {
            // message_delta.usage carries cumulative final values per Anthropic's spec —
            // server-side tools (web search, etc.) inflate input/cache counts mid-stream,
            // so we overwrite each field present rather than relying on message_start alone.
            // Without message_start observed first, we drop the data: a bare message_delta
            // would record TokenUsage{input:0, output:N} into usage_log, corrupting billing.
            if let Some(usage) = event.get("usage") {
                if !usage_buffer.seen {
                    tracing::warn!("anthropic message_delta without preceding message_start — dropping usage");
                    return;
                }
                if let Some(n) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.output_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.input_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_creation_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
                if let Some(n) = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_read_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
            }
        }
        Some("content_block_start")
            if event
                .get("content_block")
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("thinking") =>
        {
            thinking.in_block = true;
            emit_thinking("<thinking>".to_string());
        }
        Some("content_block_stop") if thinking.in_block => {
            emit_thinking("</thinking>".to_string());
            thinking.blocks.push(opex_types::ThinkingBlock {
                thinking: std::mem::take(&mut thinking.content),
                signature: std::mem::take(&mut thinking.signature),
            });
            thinking.in_block = false;
        }
        Some("content_block_delta") => {
            let delta = event.get("delta");
            match delta.and_then(|d| d.get("type")).and_then(|t| t.as_str()) {
                Some("text_delta") => {
                    if let Some(text) = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                        emit_text(text.to_string());
                    }
                }
                Some("thinking_delta") => {
                    if let Some(text) = delta.and_then(|d| d.get("thinking")).and_then(|t| t.as_str()) {
                        thinking.content.push_str(text);
                        emit_thinking(text.to_string());
                    }
                }
                Some("signature_delta") => {
                    if let Some(sig) = delta.and_then(|d| d.get("signature")).and_then(|s| s.as_str()) {
                        thinking.signature.push_str(sig);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Test helper that mirrors production behavior: emit_thinking is discarded (as in chat_stream),
/// emit_text goes to text_chunks. Returns (text_chunks, thinking_chunks, blocks) where
/// thinking_chunks captures what emit_thinking would have sent (for assertion purposes only).
#[cfg(test)]
fn process_sse_events_for_test(
    lines: &[String],
) -> (Vec<String>, Vec<String>, Vec<opex_types::ThinkingBlock>) {
    use std::cell::RefCell;
    let text_chunks: RefCell<Vec<String>> = RefCell::new(vec![]);
    let thinking_chunks: RefCell<Vec<String>> = RefCell::new(vec![]);
    let mut thinking = ThinkingState::default();
    let mut usage_buffer = StreamingAnthropicUsage::default();

    for line in lines {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => continue,
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        process_sse_event(
            &event,
            &mut thinking,
            &mut usage_buffer,
            |chunk| thinking_chunks.borrow_mut().push(chunk),
            |chunk| text_chunks.borrow_mut().push(chunk),
        );
    }
    (text_chunks.into_inner(), thinking_chunks.into_inner(), thinking.blocks)
}

#[cfg(test)]
pub(super) fn parse_streaming_usage_for_test(lines: &[String]) -> Option<opex_types::TokenUsage> {
    let mut thinking = ThinkingState::default();
    let mut usage_buffer = StreamingAnthropicUsage::default();

    for line in lines {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => continue,
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        process_sse_event(
            &event,
            &mut thinking,
            &mut usage_buffer,
            |_| {},
            |_| {},
        );
    }

    usage_buffer.into_token_usage()
}

#[cfg(test)]
mod streaming_thinking_tests {
    use super::*;

    fn make_sse_line(json: &str) -> String {
        format!("data: {json}")
    }

    #[test]
    fn streaming_emits_thinking_tags_and_populates_thinking_blocks() {
        let events = vec![
            make_sse_line(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#),
            make_sse_line(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me reason..."}}"#),
            make_sse_line(r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc123"}}"#),
            make_sse_line(r#"{"type":"content_block_stop","index":0}"#),
            make_sse_line(r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#),
            make_sse_line(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Answer here."}}"#),
            make_sse_line(r#"{"type":"content_block_stop","index":1}"#),
        ];

        let (text_chunks, thinking_chunks, blocks) = process_sse_events_for_test(&events);

        // Text stream (what the UI receives): only actual text, no thinking fragments
        assert!(
            text_chunks.iter().any(|c| c.contains("Answer here")),
            "text stream missing answer; got {text_chunks:?}"
        );
        assert!(
            !text_chunks.iter().any(|c| c.contains("thinking")),
            "text stream must not contain thinking fragments; got {text_chunks:?}"
        );

        // Thinking stream (discarded in production, collected here for assertion)
        assert!(
            thinking_chunks.contains(&"<thinking>".to_string()),
            "missing <thinking> open tag; got {thinking_chunks:?}"
        );
        assert!(
            thinking_chunks.iter().any(|c| c.contains("Let me reason")),
            "missing thinking content; got {thinking_chunks:?}"
        );
        assert!(
            thinking_chunks.contains(&"</thinking>".to_string()),
            "missing </thinking> close tag; got {thinking_chunks:?}"
        );

        // Structured thinking blocks
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].thinking, "Let me reason...");
        assert_eq!(blocks[0].signature, "abc123");
    }
}

#[cfg(test)]
mod golden_fixtures {
    use super::*;

    /// Regression: Anthropic content_block_delta of type "thinking" must
    /// route the thinking content to the thinking sink (not text), and the
    /// usage parser (which has no message_start event in this fixture) must
    /// return None rather than crash.
    #[test]
    fn content_block_delta_thinking_parses() {
        let lines = vec![
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#.to_string(),
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm..."}}"#.to_string(),
            r#"data: {"type":"content_block_stop","index":0}"#.to_string(),
        ];
        // Usage parser: no message_start in fixture, must yield None (not panic, not Some).
        assert!(
            parse_streaming_usage_for_test(&lines).is_none(),
            "no message_start event => no usage"
        );
        // Stream processor: thinking content must reach the thinking sink, not text.
        let (text, thinking, _blocks) = process_sse_events_for_test(&lines);
        assert!(text.is_empty(), "no text chunks expected, got {text:?}");
        assert_eq!(
            thinking,
            vec!["<thinking>".to_string(), "Hmm...".to_string(), "</thinking>".to_string()],
            "thinking_delta must be routed to the thinking sink with open/close markers"
        );
    }
}
