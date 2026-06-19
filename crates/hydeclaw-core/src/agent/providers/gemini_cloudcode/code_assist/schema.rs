//! Gemini tool-parameter schema sanitisation.
//!
//! Gemini's `FunctionDeclaration.parameters` schema is a strict subset of
//! JSON Schema — several common keys are rejected with 400 errors. This
//! module provides `sanitize_gemini_tool_parameters` which strips the
//! unsupported keys before the schema is sent to the API.
//!
//! Implemented in Module 2 Task 2 (schema sanitisation phase).

// Stub — full implementation arrives in M2 T2.
#![allow(dead_code)]

use serde_json::Value;

/// Strip unsupported JSON Schema keys from a Gemini tool-parameters object.
///
/// Gemini rejects schemas that contain keys outside its allowed subset.
/// This function recursively removes those keys so the schema passes
/// Gemini's server-side validation.
///
/// Keys removed: `"$schema"`, `"additionalProperties"`, `"default"`,
/// `"examples"`, `"exclusiveMinimum"`, `"exclusiveMaximum"`.
///
/// # Panics
/// Never panics — operates on owned `Value` clones.
pub fn sanitize_gemini_tool_parameters(schema: Value) -> Value {
    // Placeholder — full implementation arrives in M2 T2.
    schema
}
