//! Build the JSON body for Gemini API calls from messages + tool definitions.
//!
//! `messages_to_gemini_format` is `pub(super)` and re-exported from `mod.rs`
//! so the cross-module `#[cfg(test)] use google::messages_to_gemini_format`
//! in `providers/mod.rs` keeps working.

use hydeclaw_types::{Message, MessageRole};

/// Convert internal messages to Gemini `contents` format.
pub(in crate::agent::providers) fn messages_to_gemini_format(messages: &[Message]) -> (Option<String>, Vec<serde_json::Value>) {
    let system = messages
        .iter()
        .find(|m| m.role == MessageRole::System)
        .map(|m| m.content.clone());

    let contents: Vec<serde_json::Value> = messages
        .iter()
        .filter(|m| m.role != MessageRole::System)
        .map(|msg| {
            let role = match msg.role {
                MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "model",
                _ => "user",
            };

            if msg.role == MessageRole::Tool {
                // Tool result → functionResponse part
                return serde_json::json!({
                    "role": role,
                    "parts": [{
                        "functionResponse": {
                            "name": msg.tool_call_id.as_ref().map(|id| id.as_str()).unwrap_or("unknown"),
                            "response": {
                                "result": msg.content,
                            }
                        }
                    }]
                });
            }

            if msg.role == MessageRole::Assistant
                && let Some(ref tool_calls) = msg.tool_calls
                && !tool_calls.is_empty()
            {
                let mut parts: Vec<serde_json::Value> = Vec::new();
                if !msg.content.is_empty() {
                    parts.push(serde_json::json!({"text": msg.content}));
                }
                for tc in tool_calls {
                    let mut part = serde_json::json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    });
                    // Echo back thought_signature so Gemini 3.x thinking
                    // mode can continue its reasoning chain on the next turn.
                    // The signature lives at the Part level (sibling of
                    // functionCall), not inside it.
                    if let Some(ref sig) = tc.thought_signature {
                        part["thoughtSignature"] = serde_json::Value::String(sig.clone());
                    }
                    parts.push(part);
                }
                return serde_json::json!({"role": role, "parts": parts});
            }

            serde_json::json!({
                "role": role,
                "parts": [{"text": msg.content}]
            })
        })
        .collect();

    (system, contents)
}

/// Recursively strip `"required": []` from JSON schemas.
/// Google's Gemini API rejects empty required arrays.
pub(in crate::agent::providers) fn strip_empty_required(value: &mut serde_json::Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.retain(|k, v| !(k == "required" && v.as_array().is_some_and(std::vec::Vec::is_empty)));
        for v in obj.values_mut() {
            strip_empty_required(v);
        }
    } else if let Some(arr) = value.as_array_mut() {
        for v in arr.iter_mut() {
            strip_empty_required(v);
        }
    }
}

/// Recursively repair JSON-Schema quirks Gemini's API rejects:
///   * `type: "array"` without `items` → add `items: { "type": "string" }` fallback
///   * `required: [name]` where `name` ∉ `properties` → drop the unknown name
pub(in crate::agent::providers) fn repair_gemini_schema_quirks(value: &mut serde_json::Value) {
    if let Some(obj) = value.as_object_mut() {
        // Rule 1: array without items.
        if obj.get("type").and_then(|v| v.as_str()) == Some("array") && !obj.contains_key("items") {
            obj.insert("items".to_string(), serde_json::json!({"type": "string"}));
        }
        // Rule 2: required references unknown property.
        if let (Some(props), Some(req)) = (
            obj.get("properties").and_then(|v| v.as_object()).cloned(),
            obj.get_mut("required").and_then(|v| v.as_array_mut()),
        ) {
            req.retain(|r| r.as_str().is_some_and(|s| props.contains_key(s)));
        }
        for v in obj.values_mut() {
            repair_gemini_schema_quirks(v);
        }
    } else if let Some(arr) = value.as_array_mut() {
        for v in arr.iter_mut() {
            repair_gemini_schema_quirks(v);
        }
    }
}

/// Recursively strip JSON-Schema keys Gemini's API rejects:
/// `$defs`, `$ref`, `$schema`, `additionalProperties`, `examples`, `default`.
/// Mirrors the sanitization done by `gemini_cloudcode::code_assist::schema`
/// so the REST `google` provider can also send `tool_definitions` without
/// 400 errors on schemas produced by upstream agents (e.g. MCP tools).
pub(in crate::agent::providers) fn strip_gemini_unsupported_keys(value: &mut serde_json::Value) {
    // Gemini accepts only a minimal JSON-Schema subset (per Google's docs).
    // Drop everything outside that subset to avoid INVALID_ARGUMENT 400s.
    // Allowed: type, format, description, nullable, enum, properties,
    // required, items, minimum, maximum, minItems, maxItems.
    const FORBIDDEN: &[&str] = &[
        "$defs", "$ref", "$schema", "$id", "$anchor", "$comment",
        "additionalProperties", "patternProperties", "propertyNames",
        "examples", "default", "const",
        "oneOf", "anyOf", "allOf", "not",
        "exclusiveMinimum", "exclusiveMaximum", "multipleOf",
        "pattern", "contentEncoding", "contentMediaType",
        "dependencies", "dependentRequired", "dependentSchemas",
        "contains", "minContains", "maxContains",
        "if", "then", "else",
        "unevaluatedItems", "unevaluatedProperties",
        "readOnly", "writeOnly", "deprecated",
        "title", "examples",
        "minLength", "maxLength",  // Gemini ignores; some bindings 400 on them
        "discriminator",
    ];
    if let Some(obj) = value.as_object_mut() {
        obj.retain(|k, _| !FORBIDDEN.contains(&k.as_str()));
        for v in obj.values_mut() {
            strip_gemini_unsupported_keys(v);
        }
    } else if let Some(arr) = value.as_array_mut() {
        for v in arr.iter_mut() {
            strip_gemini_unsupported_keys(v);
        }
    }
}
