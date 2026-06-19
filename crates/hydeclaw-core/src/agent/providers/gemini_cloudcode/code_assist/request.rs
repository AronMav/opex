//! OpenAI → Gemini Code Assist request translation.
//!
//! Converts internal `Message` / `ToolDefinition` slices into the
//! Gemini `generateContent` wire format expected by
//! `POST {CODE_ASSIST_ENDPOINT}/v1internal/projects/{project}/...`.
//!
//! Full implementation arrives in Module 2 Task 3.

// Stub — full implementation arrives in M2 T3.
#![allow(dead_code)]

use serde_json::Value;

use crate::agent::providers::gemini_cloudcode::code_assist::types::ProjectContext;

/// Build the full Gemini `generateContent` JSON body from OpenAI-style
/// messages and tools.
///
/// Placeholder — returns an empty object until M2 T3 fills in the logic.
pub fn build_gemini_request(
    _messages: &[hydeclaw_types::Message],
    _tools: &[hydeclaw_types::ToolDefinition],
    _model: &str,
) -> Value {
    serde_json::json!({})
}

/// Wrap a Gemini request body in the Code Assist outer envelope.
///
/// The Code Assist API requires the `generateContent` payload to be nested
/// inside a `request` key alongside metadata fields.
///
/// Placeholder — returns the inner body unwrapped until M2 T3.
pub fn wrap_code_assist_request(inner: Value, _ctx: &ProjectContext) -> Value {
    inner
}
