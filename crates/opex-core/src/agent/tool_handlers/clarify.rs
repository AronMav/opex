use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::clarify_manager::ClarifyOutcome;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

// ── Constants ────────────────────────────────────────────────────────────────

/// Default timeout for waiting for a user response (2 minutes).
const DEFAULT_CLARIFY_TIMEOUT_SECS: u64 = 120;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns true when the execution context has an interactive channel
/// (web UI or a Telegram/channel chat_id). Returns false for inter-agent,
/// cron, and headless contexts where a blocking question would never be
/// answered.
pub fn channel_available(ctx: &Value) -> bool {
    // chat_id present and non-empty/non-null → a Telegram or channel context
    if let Some(chat_id) = ctx.get("chat_id").and_then(|v| v.as_str()) {
        if !chat_id.is_empty() && chat_id != "null" {
            return true;
        }
    }
    // _channel == "ui" → web SSE context
    ctx.get("_channel").and_then(|v| v.as_str()) == Some("ui")
}

/// Port of Hermes `_flatten_choice`: extracts a string label from each element
/// (plain strings or dicts with label/description/text/title), then caps at 4.
pub fn normalize_choices(raw: &Value) -> Vec<String> {
    let Some(arr) = raw.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|c| match c {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Object(o) => ["label", "description", "text", "title"]
                .iter()
                .find_map(|k| {
                    o.get(*k)
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                })
                .filter(|s| !s.is_empty()),
            _ => None,
        })
        .take(4)
        .collect()
}

// ── Handler ──────────────────────────────────────────────────────────────────

pub struct ClarifyHandler;

#[async_trait]
impl SystemToolHandler for ClarifyHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        // 1. Validate question
        let question = match args.get("question").and_then(|v| v.as_str()) {
            Some(q) if !q.trim().is_empty() => q.trim().to_string(),
            _ => {
                return json!({"error": "clarify: 'question' is required and must be non-empty"})
                    .to_string();
            }
        };

        // 2. Channel detection — bail early without registering a waiter
        let ctx = args.get("_context").cloned().unwrap_or(json!({}));
        if !channel_available(&ctx) {
            return json!({"error": "clarify not available in this execution context"}).to_string();
        }

        // 3. Choices normalisation + session guard
        let choices = normalize_choices(args.get("choices").unwrap_or(&json!(null)));

        let session_id = match deps.session_id {
            Some(id) => id,
            None => {
                return json!({"error": "clarify not available in this execution context"})
                    .to_string();
            }
        };

        // 4. Register waiter: awaiting_text = true when no choices offered
        let (clarify_id, rx) = deps
            .cfg
            .clarify_manager
            .register(session_id, choices.is_empty());

        // 5. Delivery — web SSE + channel (Task 5/6)
        // delivery: Task 5/6 will wire SSE event emission and Telegram button delivery here.
        // For now we log so the clarify_id is traceable.
        tracing::info!(
            clarify_id = %clarify_id,
            session_id = %session_id,
            choices = ?choices,
            question = %question,
            "clarify: awaiting user response (delivery: Task 5/6)"
        );

        // 6. Wait for response
        let timeout = Duration::from_secs(DEFAULT_CLARIFY_TIMEOUT_SECS);
        let outcome = deps
            .cfg
            .clarify_manager
            .wait_rx(rx, session_id, timeout)
            .await;

        // 7. Build result
        match outcome {
            ClarifyOutcome::Answered(answer) => json!({
                "question": question,
                "choices_offered": choices,
                "user_response": answer,
            })
            .to_string(),
            ClarifyOutcome::NoResponse(_) => json!({
                "question": question,
                "user_response": "",
                "note": "user did not respond; proceed with a reasonable default",
            })
            .to_string(),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn channel_available_true_for_chat_id_or_ui() {
        assert!(channel_available(&json!({"chat_id": "123"})));
        assert!(channel_available(&json!({"_channel": "ui"})));
    }

    #[test]
    fn channel_available_false_for_cron_or_inter_agent() {
        assert!(!channel_available(&json!({"_channel": "inter_agent"})));
        assert!(!channel_available(&json!({})));
    }

    #[test]
    fn normalize_choices_flattens_and_caps() {
        let v = json!(["a", {"label": "b"}, {"description": "c"}, "d", "e"]);
        // 5 elements but capped at 4
        assert_eq!(normalize_choices(&v), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn normalize_choices_empty_for_none() {
        assert!(normalize_choices(&json!(null)).is_empty());
    }

    #[test]
    fn normalize_choices_skips_empty_strings() {
        let v = json!(["  ", "valid", {"label": ""}, {"text": "ok"}]);
        assert_eq!(normalize_choices(&v), vec!["valid", "ok"]);
    }

    #[test]
    fn channel_available_rejects_null_chat_id_string() {
        // chat_id == "null" is treated as missing
        assert!(!channel_available(&json!({"chat_id": "null"})));
        assert!(!channel_available(&json!({"chat_id": ""})));
    }
}
