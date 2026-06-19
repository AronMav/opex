//! Non-streaming Gemini response types.
//!
//! Parsing of `GeminiResponse` into `LlmResponse` is inlined in `mod.rs::chat`
//! (and the streaming counterpart in `chat_stream`), so this module only
//! exposes the deserialization shapes.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct GeminiResponse {
    pub(super) candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    pub(super) usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiCandidate {
    pub(super) content: Option<GeminiContent>,
    #[serde(rename = "finishReason")]
    pub(super) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiContent {
    pub(super) parts: Option<Vec<GeminiPart>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiPart {
    pub(super) text: Option<String>,
    #[serde(rename = "functionCall")]
    pub(super) function_call: Option<GeminiFunctionCall>,
    /// Gemini 3.x thinking mode: opaque base64 token attached to functionCall
    /// parts. Must be echoed back on the next turn in the same part.
    #[serde(rename = "thoughtSignature", default)]
    pub(super) thought_signature: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiFunctionCall {
    pub(super) name: String,
    pub(super) args: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiUsage {
    #[serde(rename = "promptTokenCount")]
    pub(super) prompt_token_count: Option<u32>,
    #[serde(rename = "candidatesTokenCount")]
    pub(super) candidates_token_count: Option<u32>,
    #[serde(rename = "thoughtsTokenCount", default)]
    pub(super) thoughts_token_count: Option<u32>,
    #[serde(rename = "cachedContentTokenCount", default)]
    pub(super) cached_content_token_count: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: Gemini response with `safetyRatings` block must parse
    /// without erroring (the field is ignored, not required).
    #[test]
    fn safety_ratings_block_does_not_crash_parser() {
        let raw = r#"{
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hi"}]},
                "safetyRatings": [
                    {"category": "HARM_CATEGORY_HARASSMENT", "probability": "NEGLIGIBLE"}
                ]
            }],
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 1, "totalTokenCount": 6}
        }"#;
        let parsed: GeminiResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.candidates.as_ref().map(|c| c.len()), Some(1));
    }
}
