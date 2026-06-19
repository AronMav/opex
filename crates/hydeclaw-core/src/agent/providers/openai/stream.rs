//! Streaming SSE chunk handling for OpenAI-compatible chat completions.
//! Accumulates `delta`-shaped chunks into a final `LlmResponse`.

use serde::{Deserialize, Deserializer};
use super::response::ChatUsage;

/// Treat an explicit JSON `null` as `T::default()`.
///
/// `#[serde(default)]` alone handles a MISSING field, but not an explicit
/// `"field": null`. Some OpenAI-compatible backends (e.g. Xiaomi MiMo) emit
/// `"tool_calls": null` on every streaming chunk instead of omitting the field,
/// which makes our `Vec<…>`-typed deserialization fail with
/// `invalid type: null, expected a sequence`.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// Internal buffer for usage data captured across streaming chunks.
///
/// OpenAI sends usage only in the final chunk of a streaming response (when
/// `stream_options.include_usage = true`), but we use a buffer instead of
/// directly building `TokenUsage` because the chunk-handler loop is in a
/// different scope from `LlmResponse` construction.
///
/// Replaces a 5-tuple at multiple sites — named fields prevent positional-confusion
/// bugs (e.g. swapping `cache_read` and `cache_creation` at construction).
#[derive(Default)]
pub(super) struct StreamingUsage {
    pub(super) input: u32,
    pub(super) output: u32,
    pub(super) cache_read: Option<u32>,
    pub(super) cache_creation: Option<u32>,
    pub(super) reasoning: Option<u32>,
}

impl From<StreamingUsage> for hydeclaw_types::TokenUsage {
    fn from(u: StreamingUsage) -> Self {
        Self {
            input_tokens: u.input,
            output_tokens: u.output,
            cache_read_tokens: u.cache_read,
            cache_creation_tokens: u.cache_creation,
            reasoning_tokens: u.reasoning,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamChunk {
    #[serde(default)]
    pub(super) choices: Vec<StreamChoice>,
    pub(super) usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamChoice {
    pub(super) delta: StreamDelta,
    pub(super) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamDelta {
    pub(super) content: Option<String>,
    /// DeepSeek extended thinking — must be captured and passed back on subsequent turns.
    pub(super) reasoning_content: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub(super) tool_calls: Vec<StreamToolCallDelta>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamToolCallDelta {
    pub(super) index: usize,
    pub(super) id: Option<String>,
    pub(super) function: Option<StreamFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamFunctionDelta {
    pub(super) name: Option<String>,
    pub(super) arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_usage_to_token_usage_includes_cache_fields() {
        let s = StreamingUsage {
            input: 100,
            output: 50,
            cache_read: Some(30),
            cache_creation: None,
            reasoning: None,
        };
        let tu: hydeclaw_types::TokenUsage = s.into();
        assert_eq!(tu.input_tokens, 100);
        assert_eq!(tu.output_tokens, 50);
        assert_eq!(tu.cache_read_tokens, Some(30));
    }
}
