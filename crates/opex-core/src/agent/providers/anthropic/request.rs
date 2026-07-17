//! Build the JSON body for Anthropic `POST /v1/messages` from a slice
//! of `Message` plus `CallOptions`. Pure — no HTTP, no `await`,
//! no `self.client`. Kept in its own file so the streaming/non-streaming
//! call paths in `mod.rs` stay focused on transport concerns.

use super::{AnthropicProvider, CallOptions, Message, MessageRole, ToolDefinition};
use super::thinking::thinking_config;

impl AnthropicProvider {
    pub(super) fn build_request_body(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        opts: CallOptions,
    ) -> (Option<String>, serde_json::Value) {
        // Extract system message
        let system_text: Option<String> = messages
            .iter()
            .find(|m| m.role == MessageRole::System)
            .map(|m| m.content.clone());

        // Convert messages (skip system — it's a separate field)
        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .filter(|m| m.role != MessageRole::System)
            .map(|msg| {
                match msg.role {
                    MessageRole::Assistant => {
                        let has_tools = msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());
                        let has_thinking = !msg.thinking_blocks.is_empty();

                        if has_tools || has_thinking {
                            let mut content: Vec<serde_json::Value> = Vec::new();
                            // Thinking blocks MUST come before text and tool_use (Anthropic API requirement)
                            for tb in &msg.thinking_blocks {
                                content.push(serde_json::json!({
                                    "type": "thinking",
                                    "thinking": tb.thinking,
                                    "signature": tb.signature,
                                }));
                            }
                            if !msg.content.is_empty() {
                                content.push(serde_json::json!({"type": "text", "text": msg.content}));
                            }
                            if let Some(ref tool_calls) = msg.tool_calls {
                                for tc in tool_calls {
                                    content.push(serde_json::json!({
                                        "type": "tool_use",
                                        "id": tc.id,
                                        "name": tc.name,
                                        "input": tc.arguments,
                                    }));
                                }
                            }
                            serde_json::json!({"role": "assistant", "content": content})
                        } else {
                            serde_json::json!({"role": "assistant", "content": msg.content})
                        }
                    }
                    MessageRole::Tool => {
                        serde_json::json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": msg.tool_call_id.as_ref().map(|id| id.as_str()).unwrap_or(""),
                                "content": msg.content,
                            }]
                        })
                    }
                    _ => {
                        // User
                        serde_json::json!({"role": "user", "content": msg.content})
                    }
                }
            })
            .collect();

        let effective_max_tokens = self.max_tokens.unwrap_or(8_192);
        // Wave-2 Task 12: a per-turn override (from CallOptions) wins over the
        // provider's configured/runtime-override model. Never mutates
        // `self.model` — the override is scoped to this single request body.
        let effective_model = opts
            .model_override
            .clone()
            .unwrap_or_else(|| self.model.effective());
        let temperature = if opts.thinking_level > 0 {
            self.temperature.max(1.0)
        } else {
            self.temperature
        };

        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": api_messages,
            "max_tokens": effective_max_tokens,
            "temperature": temperature,
        });

        if let Some(thinking_json) = thinking_config(opts.thinking_level, &effective_model, effective_max_tokens) {
            body["thinking"] = thinking_json;
        }

        if let Some(ref sys) = system_text {
            if self.prompt_cache {
                // CACHE-02: when CLAUDE.md is provided alongside the system
                // prompt, emit it as a SECOND content block with its own
                // cache_control. Empty/whitespace claude_md is treated as
                // absent (defensive — context_builder filters empties via
                // load_claude_md, but the provider should not rely on it).
                let claude_md_present = opts
                    .claude_md_content
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);

                if claude_md_present {
                    let claude_md = opts.claude_md_content.as_deref().unwrap();
                    body["system"] = serde_json::json!([
                        {
                            "type": "text",
                            "text": sys,
                            "cache_control": {"type": "ephemeral"}
                        },
                        {
                            "type": "text",
                            "text": claude_md,
                            "cache_control": {"type": "ephemeral"}
                        }
                    ]);
                } else {
                    // Plan-01 single-block path (no CLAUDE.md / non-base agent).
                    body["system"] = serde_json::json!([{
                        "type": "text",
                        "text": sys,
                        "cache_control": {"type": "ephemeral"}
                    }]);
                }
            } else {
                // No caching → plain string. CLAUDE.md is intentionally
                // dropped from this path; the non-cache call sites
                // (openai_compat, subagent_runner) call
                // `load_workspace_prompt` which already inlines CLAUDE.md
                // into the monolithic prompt.
                body["system"] = serde_json::Value::String(sys.clone());
            }
        }

        if !tools.is_empty() {
            let mut tools_json: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            // CACHE-01 / Pitfall 1.2: stamp cache_control on the last STABLE tool —
            // i.e. the last tool whose name appears in the system tool catalogue.
            // YAML and MCP tools come AFTER system tools and vary per turn (penalty
            // sort, on-demand load), so stamping the last element of tools_json would mark
            // a per-turn-mutable tool and produce zero cache hits on turn 2+.
            //
            // If no system tool is present (only YAML/MCP), no breakpoint is added —
            // correct: there is nothing stable to cache. The system-message
            // breakpoint above (lines 287-299) still fires.
            if self.prompt_cache {
                let system_names = crate::agent::pipeline::tool_defs::all_system_tool_names();
                let last_stable_idx = tools
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, t)| system_names.contains(&t.name.as_str()))
                    .map(|(i, _)| i);
                if let Some(idx) = last_stable_idx {
                    tools_json[idx]["cache_control"] = serde_json::json!({"type": "ephemeral"});
                }
            }
            body["tools"] = serde_json::Value::Array(tools_json);
            // Force tool call when a skill trigger was detected in the system prompt
            if let Some(tool_name) = crate::agent::providers::forced_skill_tool(messages, tools) {
                body["tool_choice"] = serde_json::json!({"type": "tool", "name": tool_name});
            }
        }

        (system_text, body)
    }
}

// ── Wave-2 Task 12: per-turn model override tests ────────────────────────────

#[cfg(test)]
mod model_override_tests {
    use super::*;
    use crate::agent::providers::LlmProvider;
    use crate::secrets::SecretsManager;
    use std::sync::Arc;

    fn test_provider(default_model: &str) -> AnthropicProvider {
        AnthropicProvider::for_tests(
            default_model.to_string(),
            0.7,
            Some(1024),
            Arc::new(SecretsManager::new_noop()),
        )
    }

    fn user_msg(content: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    // NOTE: `#[tokio::test]` (not plain `#[test]`) — `SecretsManager::new_noop()`
    // builds a lazy sqlx pool that requires an active Tokio runtime context to
    // construct, even though nothing here actually awaits. Matches the
    // pre-existing pattern in `anthropic::mod::tests`.

    #[tokio::test]
    async fn model_override_replaces_configured_model_in_body() {
        let provider = test_provider("claude-opus-4-6");
        let opts = CallOptions {
            model_override: Some("test-override-model".to_string()),
            ..Default::default()
        };
        let (_, body) = provider.build_request_body(&[user_msg("hi")], &[], opts);
        assert_eq!(body["model"], "test-override-model");
    }

    #[tokio::test]
    async fn no_override_uses_configured_model() {
        let provider = test_provider("claude-opus-4-6");
        let (_, body) =
            provider.build_request_body(&[user_msg("hi")], &[], CallOptions::default());
        assert_eq!(body["model"], "claude-opus-4-6");
    }

    #[tokio::test]
    async fn model_override_does_not_mutate_provider_state() {
        // Non-persistence guarantee: building a request with an override must
        // NOT touch the provider's own ModelOverride RwLock — a subsequent
        // call (same turn or a different one) without an override must see
        // the original configured model, never the leaked override.
        let provider = test_provider("claude-opus-4-6");
        let opts = CallOptions {
            model_override: Some("test-override-model".to_string()),
            ..Default::default()
        };
        let _ = provider.build_request_body(&[user_msg("hi")], &[], opts);
        assert_eq!(provider.current_model(), "claude-opus-4-6");

        // A subsequent call with no override must go back to the configured model.
        let (_, body) =
            provider.build_request_body(&[user_msg("hi")], &[], CallOptions::default());
        assert_eq!(body["model"], "claude-opus-4-6");
    }
}
