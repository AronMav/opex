//! Non-streaming Anthropic response types and parser. Owns the JSON
//! shape returned by `POST /v1/messages` (non-streaming variant) and
//! the conversion into `LlmResponse`.

use hydeclaw_types::LlmResponse;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(in crate::agent::providers) struct AnthropicResponse {
    pub(in crate::agent::providers) content: Vec<AnthropicContentBlock>,
    pub(in crate::agent::providers) usage: Option<AnthropicUsage>,
    pub(in crate::agent::providers) stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(in crate::agent::providers) enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(in crate::agent::providers) struct AnthropicUsage {
    pub(in crate::agent::providers) input_tokens: u32,
    pub(in crate::agent::providers) output_tokens: u32,
    #[serde(default)]
    pub(in crate::agent::providers) cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    pub(in crate::agent::providers) cache_read_input_tokens: Option<u32>,
}

pub(in crate::agent::providers) fn parse_anthropic_response(api_resp: AnthropicResponse, model: &str) -> LlmResponse {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let mut thinking_blocks = Vec::new();

    for block in api_resp.content {
        match block {
            AnthropicContentBlock::Text { text } => {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&text);
            }
            AnthropicContentBlock::Thinking { thinking, signature } => {
                thinking_blocks.push(hydeclaw_types::ThinkingBlock { thinking, signature });
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(hydeclaw_types::ToolCall {
                    id: hydeclaw_types::ids::ToolCallId::from(id),
                    name,
                    arguments: input,
                });
            }
            AnthropicContentBlock::Other => {}
        }
    }

    let usage = api_resp.usage.map(|u| {
        if let Some(cache_read) = u.cache_read_input_tokens
            && cache_read > 0
        {
            tracing::info!(
                cache_read,
                cache_create = u.cache_creation_input_tokens,
                "anthropic cache hit"
            );
        }
        hydeclaw_types::TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_creation_tokens: u.cache_creation_input_tokens,
            reasoning_tokens: None,
        }
    });

    LlmResponse {
        content,
        tool_calls,
        usage,
        finish_reason: api_resp.stop_reason,
        model: Some(model.to_string()),
        provider: Some("anthropic".to_string()),
        fallback_notice: None,
        tools_used: vec![],
        iterations: 0,
        thinking_blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: Anthropic tool_use content block in non-streaming response.
    #[test]
    fn tool_use_block_in_response_parses() {
        let raw = r#"{
            "id": "msg_x",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet",
            "content": [
                {"type": "text", "text": "Calling tool"},
                {"type": "tool_use", "id": "toolu_1", "name": "search", "input": {"q": "rust"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let parsed: AnthropicResponse = serde_json::from_str(raw).unwrap();
        let resp = parse_anthropic_response(parsed, "claude-3-5-sonnet");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "search");
    }
}
