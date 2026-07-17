//! Non-streaming chat orchestration for OpenAI-compatible providers.
//!
//! Тело extracted из mod.rs::impl LlmProvider::chat. Никаких поведенческих
//! изменений — move-only commit. См. spec
//! docs/superpowers/specs/2026-05-19-w1-openai-refactor-design.md.

use super::{LlmResponse, Message, Result, ToolDefinition};
use super::minimax_xml::extract_minimax_xml_tool_calls;
use super::response::ChatCompletionResponse;
use super::OpenAiCompatibleProvider;

impl OpenAiCompatibleProvider {
    // reviewed: floor_char_boundary-bounded error previews — char boundaries
    #[allow(clippy::string_slice)]
    pub(super) async fn execute_chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        opts: super::super::CallOptions,
    ) -> Result<LlmResponse> {
        // Read back after build_chat_body so the reported model matches
        // exactly what was sent (honors CallOptions.model_override).
        let body = self.build_chat_body(messages, tools, false, &opts);
        let effective_model = body["model"].as_str().unwrap_or_default().to_string();

        let msg_count = messages.len();
        let tool_count = tools.len();
        tracing::info!(
            provider = %self.provider_name,
            model = %self.model,
            messages = msg_count,
            tools = tool_count,
            "calling LLM API"
        );

        const LARGE_CONTEXT_CHARS: usize = 200_000;
        let ctx_bytes = Self::context_size(messages);
        if ctx_bytes > LARGE_CONTEXT_CHARS {
            tracing::warn!(
                provider = %self.provider_name,
                model = %self.model,
                context_bytes = ctx_bytes,
                threshold = LARGE_CONTEXT_CHARS,
                "large context being sent to LLM — provider may reject with 5xx or truncate silently"
            );
        }

        let api_key = self.resolve_api_key().await;
        let effective_url = self.resolve_url().await;
        let auth_headers: Vec<(String, String)> = if api_key.is_empty() {
            Vec::new()
        } else {
            vec![("Authorization".to_string(), format!("Bearer {api_key}"))]
        };
        let body_text = self.client
            .post_json(
                &effective_url,
                &body,
                &auth_headers,
                &self.provider_name,
                crate::agent::providers::http::RETRYABLE_OPENAI,
                self.max_retries,
            )
            .await?;
        let api_resp: ChatCompletionResponse = serde_json::from_str(&body_text)
            .map_err(|e| {
                let preview_len = body_text.len().min(500);
                let preview = &body_text[..body_text.floor_char_boundary(preview_len)];
                tracing::error!(
                    provider = %self.provider_name,
                    body_preview = %preview,
                    "failed to parse LLM response"
                );
                anyhow::anyhow!("{} response parse error: {}", self.provider_name, e)
            })?;

        let choice = if let Some(c) = api_resp.choices.into_iter().next() {
            c
        } else {
            // Empty/null choices — retry with exponential backoff (1s, 3s, 9s)
            let delays = [1u64, 3, 9];
            let mut last_err = String::new();
            let mut found = None;
            let retry_key = self.resolve_api_key().await; // resolve once, reuse
            for (attempt, delay) in delays.iter().enumerate() {
                let preview_len = body_text.len().min(200);
                let preview = &body_text[..body_text.floor_char_boundary(preview_len)];
                tracing::warn!(
                    provider = %self.provider_name,
                    attempt = attempt + 1,
                    delay_s = delay,
                    body_preview = %preview,
                    "LLM returned empty/null choices, retrying"
                );
                tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;

                // Route through the transport (and cassette) instead of
                // discovery_client — this preserves retry semantics, tracing,
                // and cassette replay (Provider Issue 4/5). max_retries=1 means
                // one HTTP attempt per empty-choices retry (no nested retry).
                let retry_auth: Vec<(String, String)> = if retry_key.is_empty() {
                    Vec::new()
                } else {
                    vec![("Authorization".to_string(), format!("Bearer {retry_key}"))]
                };
                match self.client.post_json(
                    &effective_url,
                    &body,
                    &retry_auth,
                    &self.provider_name,
                    crate::agent::providers::http::RETRYABLE_OPENAI,
                    1, // single attempt — no nested retry
                ).await {
                    Ok(text) => {
                        if let Ok(parsed) = serde_json::from_str::<ChatCompletionResponse>(&text)
                            && let Some(c) = parsed.choices.into_iter().next() {
                                found = Some(c);
                                break;
                            }
                        last_err = format!("empty choices (attempt {})", attempt + 1);
                    }
                    Err(e) => { last_err = e.to_string(); }
                }
            }
            found.ok_or_else(|| anyhow::anyhow!("{} returned no choices after 3 retries: {}", self.provider_name, last_err))?
        };

        let tool_calls: Vec<_> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let tool_name = tc.function.name;
                let arguments = match crate::agent::json_repair::repair_json(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(tool = %tool_name, error = %e, "tool call JSON repair failed, using empty args");
                        serde_json::Value::Object(Default::default())
                    }
                };
                opex_types::ToolCall {
                    id: opex_types::ids::ToolCallId::from(tc.id),
                    name: tool_name,
                    arguments,
                    thought_signature: None,
                }
            })
            .collect();

        let content = choice.message.content.unwrap_or_default();

        // MiniMax sometimes leaks its internal XML tool-calling format in the text body.
        // Parse and extract any <minimax:tool_call> blocks before processing.
        let (content, xml_calls) = extract_minimax_xml_tool_calls(&content);
        let mut tool_calls = tool_calls;
        if !xml_calls.is_empty() {
            tracing::warn!(
                provider = %self.provider_name,
                count = xml_calls.len(),
                "extracted MiniMax XML tool calls from response content"
            );
            tool_calls.extend(xml_calls);
        }

        let usage = api_resp.usage.map(|u| opex_types::TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_tokens: u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            cache_creation_tokens: None, // OpenAI does not report cache writes
            reasoning_tokens: u
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens),
        });

        tracing::info!(
            provider = %self.provider_name,
            content_len = content.len(),
            tool_calls = tool_calls.len(),
            finish_reason = ?choice.finish_reason,
            input_tokens = usage.as_ref().map_or(0, |u| u.input_tokens),
            output_tokens = usage.as_ref().map_or(0, |u| u.output_tokens),
            "LLM response parsed"
        );

        Ok(LlmResponse {
            content,
            tool_calls,
            usage,
            model: Some(effective_model),
            provider: Some(self.provider_name.clone()),
            fallback_notice: None,
            finish_reason: choice.finish_reason,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        })
    }
}
