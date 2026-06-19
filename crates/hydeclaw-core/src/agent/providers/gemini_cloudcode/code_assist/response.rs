//! Gemini Code Assist response → `LlmResponse` translation.
//!
//! Input shape (Code Assist wraps the standard Gemini response in a `response` key):
//! ```json
//! {
//!   "response": {
//!     "candidates": [{ "content": { "parts": [...] }, "finishReason": "STOP" }],
//!     "usageMetadata": { "promptTokenCount": N, "candidatesTokenCount": M }
//!   }
//! }
//! ```

use hydeclaw_types::{LlmResponse, ThinkingBlock, TokenUsage, ToolCall};
use hydeclaw_types::ids::ToolCallId;
use serde_json::Value;

/// Translate a Gemini Code Assist `generateContent` response to `LlmResponse`.
///
/// Never returns an error — missing or malformed fields degrade gracefully
/// (empty content, no tool calls, finish_reason = "stop", no usage).
pub fn translate_gemini_response(resp: Value) -> LlmResponse {
    // Code Assist wraps the standard Gemini response in a `response` key.
    let inner = resp.get("response").unwrap_or(&resp);

    let candidates = inner
        .get("candidates")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut thinking_blocks: Vec<ThinkingBlock> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut tool_call_index: usize = 0;

    if let Some(candidate) = candidates.into_iter().next() {
        finish_reason = Some(translate_finish_reason(
            candidate
                .get("finishReason")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        ));

        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    content.push_str(text);
                } else if let Some(fc) = part.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let arguments = fc
                        .get("args")
                        .cloned()
                        .unwrap_or(Value::Object(Default::default()));
                    let id = ToolCallId::from(format!("call_{tool_call_index}"));
                    tool_call_index += 1;
                    tool_calls.push(ToolCall { id, name, arguments });
                } else if let Some(sig) = part.get("thoughtSignature").and_then(|s| s.as_str()) {
                    thinking_blocks.push(ThinkingBlock {
                        thinking: String::new(),
                        signature: sig.to_string(),
                    });
                }
            }
        }
    }

    let usage = inner.get("usageMetadata").map(|u| TokenUsage {
        input_tokens: u
            .get("promptTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        output_tokens: u
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        reasoning_tokens: None,
    });

    LlmResponse {
        content,
        tool_calls,
        usage,
        finish_reason: finish_reason.or_else(|| Some("stop".to_string())),
        model: None,
        provider: Some("gemini-cloudcode".to_string()),
        fallback_notice: None,
        tools_used: vec![],
        iterations: 0,
        thinking_blocks,
    }
}

/// Map a Gemini `finishReason` string to an OpenAI-style finish reason.
fn translate_finish_reason(reason: &str) -> String {
    match reason {
        "STOP" => "stop",
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" => "content_filter",
        _ => "stop",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parts_concatenated_into_message_content() {
        let resp = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [
                            {"text": "Hello, "},
                            {"text": "world!"}
                        ]
                    },
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 10,
                    "candidatesTokenCount": 5
                }
            }
        });
        let result = translate_gemini_response(resp);
        assert_eq!(result.content, "Hello, world!");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    #[allow(non_snake_case)]
    fn functionCall_parts_become_tool_calls() {
        let resp = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [
                            {
                                "functionCall": {
                                    "name": "search_tool",
                                    "args": {"query": "rust programming"}
                                }
                            }
                        ]
                    },
                    "finishReason": "STOP"
                }]
            }
        });
        let result = translate_gemini_response(resp);
        assert_eq!(result.content, "");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "search_tool");
        assert_eq!(result.tool_calls[0].arguments["query"], "rust programming");
        // id is synthesized as call_0
        assert_eq!(result.tool_calls[0].id.as_str(), "call_0");
    }

    #[test]
    #[allow(non_snake_case)]
    fn thoughtSignature_becomes_thinking_block() {
        let resp = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [
                            {"thoughtSignature": "base64-encoded-thinking"},
                            {"text": "Final answer"}
                        ]
                    },
                    "finishReason": "STOP"
                }]
            }
        });
        let result = translate_gemini_response(resp);
        assert_eq!(result.content, "Final answer");
        assert_eq!(result.thinking_blocks.len(), 1);
        assert_eq!(result.thinking_blocks[0].signature, "base64-encoded-thinking");
    }

    #[test]
    fn finish_reason_mapping_table() {
        let cases = [
            ("STOP", "stop"),
            ("MAX_TOKENS", "length"),
            ("SAFETY", "content_filter"),
            ("RECITATION", "content_filter"),
            ("OTHER", "stop"),
            ("UNKNOWN", "stop"),
        ];
        for (gemini_reason, expected) in cases {
            let resp = json!({
                "response": {
                    "candidates": [{
                        "content": {"role": "model", "parts": [{"text": "x"}]},
                        "finishReason": gemini_reason
                    }]
                }
            });
            let result = translate_gemini_response(resp);
            assert_eq!(
                result.finish_reason.as_deref(),
                Some(expected),
                "gemini reason {gemini_reason} should map to {expected}"
            );
        }
    }

    #[test]
    #[allow(non_snake_case)]
    fn usage_extracted_from_usageMetadata() {
        let resp = json!({
            "response": {
                "candidates": [{
                    "content": {"role": "model", "parts": [{"text": "ok"}]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 100,
                    "candidatesTokenCount": 50,
                    "totalTokenCount": 150
                }
            }
        });
        let result = translate_gemini_response(resp);
        let usage = result.usage.expect("usage should be present");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn empty_candidates_returns_empty_response() {
        let resp = json!({ "response": { "candidates": [] } });
        let result = translate_gemini_response(resp);
        assert_eq!(result.content, "");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn mixed_text_and_tool_call_parts() {
        let resp = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [
                            {"text": "Calling tool..."},
                            {"functionCall": {"name": "my_tool", "args": {"k": "v"}}},
                            {"text": " done."}
                        ]
                    },
                    "finishReason": "STOP"
                }]
            }
        });
        let result = translate_gemini_response(resp);
        assert_eq!(result.content, "Calling tool... done.");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "my_tool");
    }

    #[test]
    fn golden_fixture_text_only() {
        let raw = include_str!("tests/fixtures/code_assist_response_text_only.json");
        let resp: serde_json::Value = serde_json::from_str(raw).expect("fixture must be valid JSON");
        let result = translate_gemini_response(resp);
        assert!(!result.content.is_empty(), "text-only fixture must have non-empty content");
        assert!(result.tool_calls.is_empty(), "text-only fixture must have no tool calls");
        assert!(result.thinking_blocks.is_empty(), "text-only fixture must have no thinking blocks");
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn golden_fixture_with_tool_calls() {
        let raw = include_str!("tests/fixtures/code_assist_response_with_tool_calls.json");
        let resp: serde_json::Value = serde_json::from_str(raw).expect("fixture must be valid JSON");
        let result = translate_gemini_response(resp);
        assert!(!result.tool_calls.is_empty(), "tool-call fixture must have at least one tool call");
        let tc = &result.tool_calls[0];
        assert!(!tc.name.is_empty(), "tool call must have a non-empty name");
        assert!(tc.arguments.is_object(), "tool call arguments must be a JSON object");
    }

    #[test]
    fn golden_fixture_with_thinking() {
        let raw = include_str!("tests/fixtures/code_assist_response_with_thinking.json");
        let resp: serde_json::Value = serde_json::from_str(raw).expect("fixture must be valid JSON");
        let result = translate_gemini_response(resp);
        assert!(!result.thinking_blocks.is_empty(), "thinking fixture must have at least one thinking block");
        assert!(!result.thinking_blocks[0].signature.is_empty(), "thinking block must have a signature");
    }
}
