//! Gemini → OpenAI response translation.
//!
//! Converts a Gemini `generateContent` response into the internal
//! `LlmResponse` / `StreamEvent` types consumed by the agent pipeline.
//!
//! Full implementation arrives in Module 2 Task 3.

// Stub — full implementation arrives in M2 T3.
#![allow(dead_code)]

use anyhow::Result;
use serde_json::Value;

/// Translate a Gemini `generateContent` JSON response into an internal
/// `LlmResponse`.
///
/// Placeholder — returns an error until M2 T3 fills in the logic.
pub fn translate_gemini_response(_response: Value, _model: &str) -> Result<hydeclaw_types::LlmResponse> {
    anyhow::bail!("translate_gemini_response: not yet implemented (M2 T3)")
}
