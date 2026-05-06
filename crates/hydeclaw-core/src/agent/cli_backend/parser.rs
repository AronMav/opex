//! CLI output parsing and message-to-prompt conversion.
//!
//! - [`parse_cli_json`] — single-shot JSON output (`--output-format json`)
//!   used by claude/gemini/codex.
//! - [`parse_cli_jsonl`] — line-delimited JSON (one event per line) for
//!   CLIs that stream incremental updates.
//! - [`format_messages_for_cli`] — flatten an LLM message list into a
//!   single user prompt + optional system prompt.

use serde::Deserialize;

use super::CliOutput;

pub(super) fn parse_cli_json(raw: &str) -> CliOutput {
    #[derive(Deserialize)]
    struct JsonOut {
        #[serde(alias = "result", alias = "response", alias = "content")]
        text: Option<String>,
        #[serde(alias = "session_id", alias = "sessionId", alias = "conversation_id")]
        session_id: Option<String>,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        input_tokens: Option<u32>,
        #[serde(default)]
        output_tokens: Option<u32>,
        #[serde(default)]
        usage: Option<serde_json::Value>,
    }

    let parsed: Option<JsonOut> = serde_json::from_str(raw.trim()).ok();
    match parsed {
        Some(p) => {
            if let Some(cost) = p.cost_usd {
                tracing::info!(cost_usd = cost, "CLI cost");
            }
            let usage = match (p.input_tokens, p.output_tokens) {
                (Some(inp), Some(out)) => Some(hydeclaw_types::TokenUsage {
                    input_tokens: inp,
                    output_tokens: out,
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                    reasoning_tokens: None,
                }),
                _ => {
                    // Try nested usage object (Anthropic CLI format includes cache fields)
                    p.usage.as_ref().and_then(|u| {
                        let inp =
                            u.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                        let out =
                            u.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                        if inp > 0 || out > 0 {
                            Some(hydeclaw_types::TokenUsage {
                                input_tokens: inp,
                                output_tokens: out,
                                cache_read_tokens: u
                                    .get("cache_read_input_tokens")
                                    .and_then(serde_json::Value::as_u64)
                                    .map(|v| v as u32),
                                cache_creation_tokens: u
                                    .get("cache_creation_input_tokens")
                                    .and_then(serde_json::Value::as_u64)
                                    .map(|v| v as u32),
                                reasoning_tokens: None,
                            })
                        } else {
                            None
                        }
                    })
                }
            };
            CliOutput {
                text: p.text.unwrap_or_default(),
                session_id: p.session_id,
                usage,
            }
        }
        None => CliOutput {
            text: raw.trim().to_string(),
            session_id: None,
            usage: None,
        },
    }
}

pub(super) fn parse_cli_jsonl(raw: &str) -> CliOutput {
    let mut texts = Vec::new();
    let mut session_id = None;
    let mut usage = None;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            // Extract session_id
            if session_id.is_none() {
                session_id = v
                    .get("session_id")
                    .or_else(|| v.get("thread_id"))
                    .and_then(|s| s.as_str())
                    .map(std::string::ToString::to_string);
            }
            // Extract text
            if let Some(text) = v
                .get("text")
                .or_else(|| v.get("result"))
                .and_then(|t| t.as_str())
            {
                texts.push(text.to_string());
            }
            if let Some(item) = v.get("item")
                && let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    texts.push(text.to_string());
                }
            // Extract usage
            if let Some(u) = v.get("usage") {
                let inp =
                    u.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                let out =
                    u.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                if inp > 0 || out > 0 {
                    usage = Some(hydeclaw_types::TokenUsage {
                        input_tokens: inp,
                        output_tokens: out,
                        cache_read_tokens: u
                            .get("cache_read_input_tokens")
                            .and_then(serde_json::Value::as_u64)
                            .map(|v| v as u32),
                        cache_creation_tokens: u
                            .get("cache_creation_input_tokens")
                            .and_then(serde_json::Value::as_u64)
                            .map(|v| v as u32),
                        reasoning_tokens: None,
                    });
                }
            }
        }
    }

    CliOutput {
        text: texts.join("\n"),
        session_id,
        usage,
    }
}

/// Format messages for CLI prompt. Returns (`user_prompt`, `system_prompt`).
pub fn format_messages_for_cli(
    messages: &[hydeclaw_types::Message],
) -> (String, Option<String>) {
    use hydeclaw_types::MessageRole;
    let mut system_parts = Vec::new();
    let mut prompt_parts = Vec::new();
    for msg in messages {
        match msg.role {
            MessageRole::System => system_parts.push(msg.content.clone()),
            MessageRole::User => prompt_parts.push(msg.content.clone()),
            MessageRole::Assistant => {
                prompt_parts.push(format!("[Assistant]\n{}", msg.content));
            }
            MessageRole::Tool => {
                prompt_parts.push(format!("[Tool result]\n{}", msg.content));
            }
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    (prompt_parts.join("\n\n"), system)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydeclaw_types::{Message, MessageRole};

    // ── parse_cli_json ──────────────────────────────────────────────────────

    #[test]
    fn parse_json_valid_result() {
        let json = r#"{"result": "Hello", "session_id": "abc-123", "input_tokens": 10, "output_tokens": 20}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "Hello");
        assert_eq!(out.session_id, Some("abc-123".to_string()));
        let u = out.usage.unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn parse_json_response_alias() {
        let json = r#"{"response": "World", "sessionId": "s-42"}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "World");
        assert_eq!(out.session_id, Some("s-42".to_string()));
        assert!(out.usage.is_none());
    }

    #[test]
    fn parse_json_content_alias() {
        let json = r#"{"content": "Hi", "conversation_id": "c-1"}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "Hi");
        assert_eq!(out.session_id, Some("c-1".to_string()));
    }

    #[test]
    fn parse_json_invalid_returns_raw() {
        let raw = "Not a JSON at all";
        let out = parse_cli_json(raw);
        assert_eq!(out.text, "Not a JSON at all");
        assert!(out.session_id.is_none());
        assert!(out.usage.is_none());
    }

    #[test]
    fn parse_json_empty_string() {
        let out = parse_cli_json("");
        assert_eq!(out.text, "");
        assert!(out.session_id.is_none());
        assert!(out.usage.is_none());
    }

    #[test]
    fn parse_json_nested_usage() {
        let json = r#"{"result": "ok", "usage": {"input_tokens": 100, "output_tokens": 50}}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "ok");
        let u = out.usage.unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
    }

    #[test]
    fn claude_cli_returns_none_for_unsupported_cache_fields() {
        // Top-level only; no nested usage object → cache fields stay None.
        let json = r#"{"result": "...", "input_tokens": 100, "output_tokens": 50}"#;
        let out = parse_cli_json(json);
        let u = out.usage.expect("usage present");
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_tokens, None);
        assert_eq!(u.cache_creation_tokens, None);
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn claude_cli_maps_cache_fields_when_nested_usage_has_them() {
        // Anthropic CLI JSON puts cache fields inside the nested `usage` object.
        let json = r#"{
            "result": "ok",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 700,
                "cache_creation_input_tokens": 200
            }
        }"#;
        let out = parse_cli_json(json);
        let u = out.usage.expect("usage present");
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_tokens, Some(700));
        assert_eq!(u.cache_creation_tokens, Some(200));
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn parse_json_cost_usd_present() {
        let json = r#"{"result": "ok", "cost_usd": 0.0123}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "ok");
    }

    #[test]
    fn parse_json_no_text_field() {
        let json = r#"{"session_id": "s-1"}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "");
        assert_eq!(out.session_id, Some("s-1".to_string()));
    }

    // ── parse_cli_jsonl ────────────────────────────────────────────────────

    #[test]
    fn parse_jsonl_multiple_lines() {
        let jsonl = "{\"text\": \"Hello\"}\n{\"text\": \"World\", \"session_id\": \"s-1\"}\n{\"usage\": {\"input_tokens\": 5, \"output_tokens\": 10}}";
        let out = parse_cli_jsonl(jsonl);
        assert_eq!(out.text, "Hello\nWorld");
        assert_eq!(out.session_id, Some("s-1".to_string()));
        let u = out.usage.unwrap();
        assert_eq!(u.input_tokens, 5);
        assert_eq!(u.output_tokens, 10);
    }

    #[test]
    fn parse_jsonl_item_text() {
        let jsonl = r#"{"item": {"text": "From item"}}"#;
        let out = parse_cli_jsonl(jsonl);
        assert_eq!(out.text, "From item");
    }

    #[test]
    fn parse_jsonl_empty() {
        let out = parse_cli_jsonl("");
        assert_eq!(out.text, "");
        assert!(out.session_id.is_none());
    }

    // ── format_messages_for_cli ────────────────────────────────────────────

    fn msg(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    #[test]
    fn format_system_and_user() {
        let msgs = vec![
            msg(MessageRole::System, "You are helpful."),
            msg(MessageRole::User, "Hello!"),
        ];
        let (prompt, system) = format_messages_for_cli(&msgs);
        assert_eq!(prompt, "Hello!");
        assert_eq!(system.as_deref(), Some("You are helpful."));
    }

    #[test]
    fn format_user_only() {
        let msgs = vec![msg(MessageRole::User, "Hi")];
        let (prompt, system) = format_messages_for_cli(&msgs);
        assert_eq!(prompt, "Hi");
        assert!(system.is_none());
    }

    #[test]
    fn format_with_assistant_and_tool() {
        let msgs = vec![
            msg(MessageRole::User, "Q1"),
            msg(MessageRole::Assistant, "A1"),
            msg(MessageRole::Tool, "T1"),
            msg(MessageRole::User, "Q2"),
        ];
        let (prompt, _system) = format_messages_for_cli(&msgs);
        assert!(prompt.contains("Q1"));
        assert!(prompt.contains("[Assistant]\nA1"));
        assert!(prompt.contains("[Tool result]\nT1"));
        assert!(prompt.contains("Q2"));
    }

    #[test]
    fn format_empty_messages() {
        let (prompt, system) = format_messages_for_cli(&[]);
        assert_eq!(prompt, "");
        assert!(system.is_none());
    }
}
