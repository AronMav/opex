//! Google Gemini API provider —
//! extracted from providers.rs for readability.

use super::{async_trait, Arc, SecretsManager, ModelOverride, Message, LlmProvider, ToolDefinition, Result, LlmResponse, mpsc, HttpTransport};

mod request;
pub(super) use request::{messages_to_gemini_format, strip_empty_required};

mod response;
use response::GeminiResponse;

// ── Google Gemini API Provider ──────────────────────────────────────────────

pub struct GoogleProvider {
    client: Arc<dyn HttpTransport>,
    streaming_client: Arc<dyn HttpTransport>,
    base_url: String,
    api_key_name: String,
    /// Vault scope for `LLM_CREDENTIALS` (provider UUID). When set, checked first.
    credential_scope: Option<String>,
    secrets: Arc<SecretsManager>,
    model: ModelOverride,
    temperature: f64,
    max_tokens: Option<u32>,
    /// Per-provider timeout knobs (connect / request / stream inactivity / stream max).
    /// Consumed by `stream_with_cancellation` in `chat_stream`.
    timeouts: super::TimeoutsConfig,
    /// Cooperative cancellation token shared with the streaming producer task.
    /// Engine-level shutdown or user cancel flips this; the stream drains and
    /// `chat_stream` surfaces a typed `LlmCallError`.
    cancel: tokio_util::sync::CancellationToken,
    /// Max HTTP retry attempts on transient errors (429/5xx). Configurable per-provider via UI.
    max_retries: u32,
}

impl GoogleProvider {
    /// Build a `GoogleProvider` from a `ProviderRow`, storing the shared
    /// `cancel` token + typed `timeouts` so `chat_stream` can thread them into
    /// `stream_with_cancellation`.
    ///
    /// HTTP clients are built via `build_provider_clients(&timeouts)` honoring
    /// `connect_secs` / `request_secs` (not the legacy 10s/120s hardcoded values).
    ///
    /// `overrides` supplies agent/route-level temperature, max_tokens, model.
    /// Resolution order: override → row default → hardcoded last-resort.
    pub(crate) fn new_from_row(
        row: &crate::db::providers::ProviderRow,
        secrets: Arc<SecretsManager>,
        timeouts: super::TimeoutsConfig,
        cancel: tokio_util::sync::CancellationToken,
        opts: super::timeouts::ProviderOptions,
        overrides: super::ProviderOverrides,
    ) -> anyhow::Result<Self> {
        let model = overrides
            .model
            .clone()
            .unwrap_or_else(|| row.default_model.clone().unwrap_or_default());
        let key_env = super::PROVIDER_TYPES
            .iter()
            .find(|pt| pt.id == row.provider_type)
            .map_or("GOOGLE_API_KEY", |pt| pt.default_secret_name);

        let (client, streaming_client) = super::build_provider_clients(&timeouts);

        let temperature = overrides.temperature.unwrap_or(0.7);
        let max_tokens = overrides.max_tokens;

        let provider = Self {
            client,
            streaming_client,
            base_url: row
                .base_url
                .clone()
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string()),
            api_key_name: key_env.to_string(),
            credential_scope: Some(row.id.to_string()),
            secrets,
            model: super::ModelOverride::new(model),
            temperature,
            max_tokens,
            timeouts,
            cancel,
            max_retries: opts.max_retries,
        };
        Ok(provider)
    }

    /// Set vault credential scope (provider UUID) for `LLM_CREDENTIALS` lookup.
    ///
    /// `new_from_row` builds the scope literally from the row UUID.
    /// Kept as a stable fluent API for external consumers.
    #[allow(dead_code)]
    pub fn with_credential_scope(mut self, scope: String) -> Self {
        self.credential_scope = Some(scope);
        self
    }

    async fn resolve_api_key(&self) -> Option<String> {
        super::resolve_credential(
            &self.secrets,
            self.credential_scope.as_deref(),
            &self.api_key_name,
        ).await
    }
}

#[async_trait]
impl LlmProvider for GoogleProvider {
    // reviewed: floor_char_boundary-bounded error preview — char boundary
    #[allow(clippy::string_slice)]
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _opts: super::CallOptions,
    ) -> Result<LlmResponse> {
        let api_key = self.resolve_api_key().await
            .ok_or_else(|| anyhow::anyhow!("Google API key not found ({})", self.api_key_name))?;

        let effective_model = self.model.effective();
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.base_url.trim_end_matches('/'),
            effective_model,
            api_key
        );

        let (system, contents) = messages_to_gemini_format(messages);

        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": {
                "temperature": self.temperature,
                "maxOutputTokens": self.max_tokens.unwrap_or(8192),
            }
        });

        if let Some(ref sys) = system {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": sys}]
            });
        }

        if !tools.is_empty() {
            // Gemini rejects duplicate function names. Dedupe by name, keeping
            // the first occurrence (this matches the precedence the engine uses
            // when resolving tool_use → tool dispatch).
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let fn_decls: Vec<serde_json::Value> = tools
                .iter()
                .filter(|t| seen.insert(t.name.clone()))
                .map(|t| {
                    let mut params = t.input_schema.clone();
                    request::strip_gemini_unsupported_keys(&mut params);
                    request::repair_gemini_schema_quirks(&mut params);
                    strip_empty_required(&mut params);
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": params,
                    })
                })
                .collect();
            body["tools"] = serde_json::json!([{"functionDeclarations": fn_decls}]);
            // Force tool call when a skill trigger was detected in the system prompt
            if let Some(tool_name) = super::forced_skill_tool(messages, tools) {
                body["toolConfig"] = serde_json::json!({
                    "functionCallingConfig": {
                        "mode": "ANY",
                        "allowedFunctionNames": [tool_name]
                    }
                });
            }
        }

        tracing::info!(
            provider = "google",
            model = %self.model,
            messages = messages.len(),
            tools = tools.len(),
            "calling Google Gemini API"
        );

        // Google uses ?key= in URL, no auth header needed
        let body_text = self.client
            .post_json(
                &url,
                &body,
                &[],
                "google",
                crate::agent::providers::http::RETRYABLE_OPENAI,
                self.max_retries,
            )
            .await?;

        let api_resp: GeminiResponse = serde_json::from_str(&body_text).map_err(|e| {
            let preview_len = body_text.len().min(500);
            let preview = &body_text[..body_text.floor_char_boundary(preview_len)];
            tracing::error!(provider = "google", body_preview = %preview, "failed to parse response");
            anyhow::anyhow!("google response parse error: {e}")
        })?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = None;

        if let Some(candidates) = api_resp.candidates
            && let Some(candidate) = candidates.into_iter().next() {
                finish_reason = candidate.finish_reason.clone();
                if let Some(c) = candidate.content
                    && let Some(parts) = c.parts {
                        for (i, part) in parts.into_iter().enumerate() {
                            if let Some(text) = part.text {
                                if !content.is_empty() {
                                    content.push('\n');
                                }
                                content.push_str(&text);
                            }
                            if let Some(fc) = part.function_call {
                                tool_calls.push(opex_types::ToolCall {
                                    id: opex_types::ids::ToolCallId::new(format!("call_{i}")),
                                    name: fc.name,
                                    arguments: fc.args.unwrap_or(serde_json::Value::Object(Default::default())),
                                    thought_signature: part.thought_signature,
                                });
                            }
                        }
                    }
            }

        let usage = api_resp.usage_metadata.map(|u| opex_types::TokenUsage {
            input_tokens: u.prompt_token_count.unwrap_or(0),
            output_tokens: u.candidates_token_count.unwrap_or(0),
            cache_read_tokens: u.cached_content_token_count,
            cache_creation_tokens: None,
            reasoning_tokens: u.thoughts_token_count,
        });

        tracing::info!(
            provider = "google",
            content_len = content.len(),
            tool_calls = tool_calls.len(),
            finish_reason = ?finish_reason,
            input_tokens = usage.as_ref().map_or(0, |u| u.input_tokens),
            output_tokens = usage.as_ref().map_or(0, |u| u.output_tokens),
            "Google response parsed"
        );

        Ok(LlmResponse {
            content,
            tool_calls,
            usage,
            finish_reason,
            model: Some(effective_model),
            provider: Some("google".to_string()),
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        })
    }

    // reviewed: offsets from find('\n')+1 (ASCII) — char boundaries
    #[allow(clippy::string_slice)]
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::Sender<String>,
        _opts: super::CallOptions,
    ) -> Result<LlmResponse> {
        if !tools.is_empty() {
            let response = self.chat(messages, tools, _opts).await?;
            if response.tool_calls.is_empty() {
                let filtered = crate::agent::thinking::strip_thinking(&response.content);
                if !filtered.is_empty() {
                    chunk_tx.send(filtered).await.ok();
                }
            }
            return Ok(response);
        }

        let api_key = self.resolve_api_key().await
            .ok_or_else(|| anyhow::anyhow!("Google API key not found ({})", self.api_key_name))?;

        let effective_model = self.model.effective();
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url.trim_end_matches('/'),
            effective_model,
            api_key
        );

        let (system, contents) = messages_to_gemini_format(messages);

        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": {
                "temperature": self.temperature,
                "maxOutputTokens": self.max_tokens.unwrap_or(8192),
            }
        });

        if let Some(ref sys) = system {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": sys}]
            });
        }

        tracing::info!(provider = "google", model = %self.model, "calling Google Gemini API (streaming)");

        let start = std::time::Instant::now();
        let resp = match self.streaming_client
            .post_json_stream(
                &url,
                &body,
                &[],
                "google",
                crate::agent::providers::http::RETRYABLE_OPENAI,
                self.max_retries,
            )
            .await
        {
            Ok(r) => r,
            Err(super::http::SendError::Network(e)) => {
                return Err(anyhow::Error::new(super::classify_reqwest_err(
                    e,
                    "google",
                    self.timeouts.connect_secs,
                    self.timeouts.request_secs,
                )));
            }
            Err(super::http::SendError::Http { status: code, body: err_text, retry_after }) => {
                if code == 401 || code == 403 {
                    return Err(anyhow::Error::new(crate::agent::providers::LlmCallError::AuthError {
                        provider: "google".to_string(),
                        status: code,
                    }));
                }
                if code >= 500 {
                    return Err(anyhow::Error::new(crate::agent::providers::LlmCallError::Server5xx {
                        provider: "google".to_string(),
                        status: code,
                    }));
                }
                if let Some(ra) = retry_after {
                    anyhow::bail!("google API error (retry-after: {ra}): {err_text}");
                }
                anyhow::bail!("google API error: {err_text}");
            }
        };

        let mut full_content = String::new();
        let mut buffer = String::new();
        let mut finish_reason: Option<String> = None;
        let mut thinking_filter = crate::agent::thinking::ThinkingFilter::new();

        use tokio_stream::StreamExt;
        use crate::agent::providers::{CancelSlot, LlmCallError, cancellable_stream::stream_with_cancellation};

        let slot = CancelSlot::new();
        let byte_stream = stream_with_cancellation(
            resp.bytes_stream(),
            self.cancel.child_token(),
            slot.clone(),
            self.timeouts,
        );
        let mut byte_stream = std::pin::pin!(byte_stream);
        while let Some(chunk_result) = StreamExt::next(&mut byte_stream).await {
            let chunk_bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    return Err(anyhow::Error::new(LlmCallError::from(e)));
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(chunk_json) = serde_json::from_str::<GeminiResponse>(data)
                        && let Some(candidates) = chunk_json.candidates {
                            for candidate in candidates {
                                if let Some(ref fr) = candidate.finish_reason {
                                    finish_reason = Some(fr.clone());
                                }
                                if let Some(c) = candidate.content
                                    && let Some(parts) = c.parts {
                                        for part in parts {
                                            if let Some(ref text) = part.text {
                                                full_content.push_str(text);
                                                let filtered = thinking_filter.process(text);
                                                if !filtered.is_empty() {
                                                    chunk_tx.send(filtered).await.ok();
                                                }
                                            }
                                        }
                                    }
                            }
                        }
            }
        }

        // Stream exited. Surface typed cancellation reason with partial state,
        // so callers can downcast and decide retry / persistence.
        if let Some(reason) = slot.get() {
            use crate::agent::providers::error::{CancelReason, PartialState};
            let partial_state = if !full_content.is_empty() {
                PartialState::Text(full_content.clone())
            } else {
                PartialState::Empty
            };
            let err = match reason {
                CancelReason::InactivityTimeout { silent_secs } => LlmCallError::InactivityTimeout {
                    provider: self.name().to_string(),
                    silent_secs,
                    partial_state,
                },
                CancelReason::MaxDurationExceeded { elapsed_secs } => LlmCallError::MaxDurationExceeded {
                    provider: self.name().to_string(),
                    elapsed_secs,
                    partial_state,
                },
                CancelReason::UserCancelled => LlmCallError::UserCancelled { partial_state },
                CancelReason::ShutdownDrain => LlmCallError::ShutdownDrain { partial_state },
            };
            return Err(anyhow::Error::new(err));
        }

        let elapsed = start.elapsed();
        tracing::info!(
            provider = "google",
            content_len = full_content.len(),
            finish_reason = ?finish_reason,
            elapsed_ms = elapsed.as_millis() as u64,
            "streaming response complete"
        );

        Ok(LlmResponse {
            content: full_content,
            tool_calls: vec![],
            usage: None,
            finish_reason,
            model: Some(effective_model),
            provider: Some("google".to_string()),
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        })
    }

    fn name(&self) -> &'static str {
        "google"
    }

    fn set_model_override(&self, model: Option<String>) {
        self.model.set(model);
    }

    fn current_model(&self) -> String {
        self.model.effective()
    }

    fn run_max_duration_secs(&self) -> u64 {
        self.timeouts.run_max_duration_secs
    }

    async fn context_limit_hint(&self, model: &str) -> Option<u32> {
        let api_key = self.resolve_api_key().await?;
        // Strip "models/" prefix if present — some callers pass the bare name.
        let model_id = model.strip_prefix("models/").unwrap_or(model);
        let url = format!(
            "{}/v1beta/models/{}?key={}",
            self.base_url.trim_end_matches('/'),
            model_id,
            api_key,
        );
        let resp = self.client
            .discovery_client()
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send().await.ok()?
            .error_for_status().ok()?
            .json::<serde_json::Value>().await.ok()?;

        if let Some(n) = resp.get("inputTokenLimit").and_then(|v| v.as_u64()) {
            tracing::debug!(model, input_token_limit = n, "google /v1beta/models inputTokenLimit");
            return Some(n as u32);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_usage_maps_thoughts_and_cached() {
        let json = r#"{
            "candidates": [],
            "usageMetadata": {
                "promptTokenCount": 1000,
                "candidatesTokenCount": 500,
                "thoughtsTokenCount": 200,
                "cachedContentTokenCount": 600
            }
        }"#;
        let resp: GeminiResponse = serde_json::from_str(json).expect("parse");
        let u = resp.usage_metadata.expect("usage_metadata present");

        assert_eq!(u.prompt_token_count, Some(1000));
        assert_eq!(u.candidates_token_count, Some(500));
        assert_eq!(u.thoughts_token_count, Some(200));
        assert_eq!(u.cached_content_token_count, Some(600));
    }
}

