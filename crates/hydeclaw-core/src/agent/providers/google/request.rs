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
                    parts.push(serde_json::json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    }));
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
