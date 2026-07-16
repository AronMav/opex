//! Anthropic Messages API provider —
//! extracted from providers.rs for readability.

use super::{async_trait, Arc, SecretsManager, ModelOverride, Message, ToolDefinition, MessageRole, LlmProvider, LlmResponse, Result, mpsc, CallOptions, HttpTransport};

mod thinking;

mod response;
pub(super) use response::{AnthropicResponse, parse_anthropic_response};
#[cfg(test)]
pub(super) use response::{AnthropicContentBlock, AnthropicUsage};

mod stream;
use stream::{StreamingAnthropicUsage, ThinkingState, process_sse_event};
#[cfg(test)]
use stream::parse_streaming_usage_for_test;

mod request;

// ── Anthropic Messages API Provider ──────────────────────────────────────────

pub struct AnthropicProvider {
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
    prompt_cache: bool,
    /// Per-provider timeout knobs (connect / request / stream inactivity / stream max).
    /// Consumed by `stream_with_cancellation` in `chat_stream`.
    timeouts: super::TimeoutsConfig,
    /// Cooperative cancellation token shared with the streaming producer task.
    /// Engine-level shutdown or user cancel flips this; the stream drains and
    /// `chat_stream` surfaces a typed `LlmCallError`.
    cancel: tokio_util::sync::CancellationToken,
    /// Max HTTP retry attempts on transient errors (429/5xx). Configurable per-provider via UI.
    max_retries: u32,
    /// Auth scheme for the Anthropic-compatible endpoint. `false` → native
    /// Anthropic `x-api-key`; `true` → `Authorization: Bearer` for third-party
    /// anthropic-compatible vendors (Kimi Code / Moonshot `/anthropic` gateway,
    /// which documents only Bearer and rejects `x-api-key`).
    bearer_auth: bool,
}

impl AnthropicProvider {
    /// Build the auth + version headers for an Anthropic-compatible request.
    /// `anthropic-version` is always sent; the credential goes as `x-api-key`
    /// for native Anthropic or `Authorization: Bearer` for third-party vendors
    /// (`bearer_auth`).
    fn auth_headers(&self, api_key: Option<&str>) -> Vec<(String, String)> {
        let mut headers = vec![("anthropic-version".to_string(), "2023-06-01".to_string())];
        if let Some(key) = api_key.filter(|k| !k.is_empty()) {
            if self.bearer_auth {
                headers.push(("authorization".to_string(), format!("Bearer {key}")));
            } else {
                headers.push(("x-api-key".to_string(), key.to_string()));
            }
        }
        headers
    }
}

impl AnthropicProvider {
    /// Build an `AnthropicProvider` from a `ProviderRow`, storing the shared
    /// `cancel` token + typed `timeouts` so `chat_stream` can thread them into
    /// `stream_with_cancellation`.
    ///
    /// HTTP clients are built via `build_provider_clients(&timeouts)` honoring
    /// `connect_secs` / `request_secs` (not the legacy 10s/120s hardcoded values).
    ///
    /// `overrides` supplies agent/route-level temperature, max_tokens, model,
    /// prompt_cache. Resolution order: override → row/opts default → hardcoded
    /// last-resort. `prompt_cache` reads `ProviderOptions.prompt_cache` when
    /// the override is `None`.
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
            .map_or("ANTHROPIC_API_KEY", |pt| pt.default_secret_name);

        let (client, streaming_client) = super::build_provider_clients(&timeouts);

        let temperature = overrides.temperature.unwrap_or(0.7);
        let max_tokens = overrides.max_tokens;
        let prompt_cache = overrides.prompt_cache.unwrap_or(opts.prompt_cache);

        let provider = Self {
            client,
            streaming_client,
            base_url: row
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
            api_key_name: key_env.to_string(),
            credential_scope: Some(row.id.to_string()),
            secrets,
            model: super::ModelOverride::new(model),
            temperature,
            max_tokens,
            prompt_cache,
            timeouts,
            cancel,
            max_retries: opts.max_retries,
            // Native Anthropic → x-api-key; any third-party anthropic-compatible
            // vendor (currently `kimi`) → Authorization: Bearer.
            bearer_auth: row.provider_type != "anthropic",
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

    /// Test-only: replace both HTTP transports (e.g. with a `CassetteTransport`
    /// for offline provider tests). Not compiled in production.
    #[cfg(test)]
    pub(crate) fn with_transports(
        mut self,
        client: Arc<dyn super::HttpTransport>,
        streaming_client: Arc<dyn super::HttpTransport>,
    ) -> Self {
        self.client = client;
        self.streaming_client = streaming_client;
        self
    }

    /// Minimal constructor for unit tests only — avoids depending on the
    /// deleted `new()` / `with_options()` paths. Not compiled in production.
    #[cfg(test)]
    pub(super) fn for_tests(
        model: String,
        temperature: f64,
        max_tokens: Option<u32>,
        secrets: Arc<SecretsManager>,
    ) -> Self {
        let (client, streaming_client) = super::build_provider_clients(&super::TimeoutsConfig::default());
        Self {
            client,
            streaming_client,
            base_url: "https://api.anthropic.com".to_string(),
            api_key_name: "ANTHROPIC_API_KEY".to_string(),
            credential_scope: None,
            secrets,
            model: ModelOverride::new(model),
            temperature,
            max_tokens,
            prompt_cache: false,
            timeouts: super::TimeoutsConfig::default(),
            cancel: tokio_util::sync::CancellationToken::new(),
            max_retries: 3,
            bearer_auth: false,
        }
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
impl LlmProvider for AnthropicProvider {
    // reviewed: floor_char_boundary-bounded error preview — char boundary
    #[allow(clippy::string_slice)]
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        opts: CallOptions,
    ) -> Result<LlmResponse> {
        let (_, body) = self.build_request_body(messages, tools, opts);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        tracing::info!(
            provider = "anthropic",
            model = %self.model,
            messages = messages.len(),
            tools = tools.len(),
            "calling Anthropic API"
        );

        let api_key = self.resolve_api_key().await;

        let auth_headers = self.auth_headers(api_key.as_deref());

        let body_text = self.client
            .post_json(
                &url,
                &body,
                &auth_headers,
                "anthropic",
                crate::agent::providers::http::RETRYABLE_ANTHROPIC,
                self.max_retries,
            )
            .await?;

        let api_resp: AnthropicResponse = serde_json::from_str(&body_text).map_err(|e| {
            let preview_len = body_text.len().min(500);
            let preview = &body_text[..body_text.floor_char_boundary(preview_len)];
            tracing::error!(provider = "anthropic", body_preview = %preview, "failed to parse response");
            anyhow::anyhow!("anthropic response parse error: {e}")
        })?;

        let effective_model = self.model.effective();
        let response = parse_anthropic_response(api_resp, &effective_model);

        tracing::info!(
            provider = "anthropic",
            content_len = response.content.len(),
            tool_calls = response.tool_calls.len(),
            input_tokens = response.usage.as_ref().map_or(0, |u| u.input_tokens),
            output_tokens = response.usage.as_ref().map_or(0, |u| u.output_tokens),
            "Anthropic response parsed"
        );

        Ok(response)
    }

    // reviewed: offsets from find('\n')+1 (ASCII) — char boundaries
    #[allow(clippy::string_slice)]
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::Sender<String>,
        opts: CallOptions,
    ) -> Result<LlmResponse> {
        if !tools.is_empty() {
            let response = self.chat(messages, tools, opts).await?;
            if response.tool_calls.is_empty() {
                let filtered = crate::agent::thinking::strip_thinking(&response.content);
                if !filtered.is_empty() {
                    chunk_tx.send(filtered).await.ok();
                }
            }
            return Ok(response);
        }

        let (_, mut body) = self.build_request_body(messages, tools, opts);
        body["stream"] = serde_json::Value::Bool(true);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        tracing::info!(provider = "anthropic", model = %self.model, "calling Anthropic API (streaming)");

        let start = std::time::Instant::now();
        let api_key = self.resolve_api_key().await;
        let auth_headers = self.auth_headers(api_key.as_deref());
        let resp = match self.streaming_client
            .post_json_stream(
                &url,
                &body,
                &auth_headers,
                "anthropic",
                crate::agent::providers::http::RETRYABLE_ANTHROPIC,
                self.max_retries,
            )
            .await
        {
            Ok(r) => r,
            Err(super::http::SendError::Network(e)) => {
                return Err(anyhow::Error::new(super::classify_reqwest_err(
                    e,
                    "anthropic",
                    self.timeouts.connect_secs,
                    self.timeouts.request_secs,
                )));
            }
            Err(super::http::SendError::Http { status: code, body: err_text, retry_after }) => {
                if code == 401 || code == 403 {
                    return Err(anyhow::Error::new(crate::agent::providers::LlmCallError::AuthError {
                        provider: "anthropic".to_string(),
                        status: code,
                    }));
                }
                if code >= 500 {
                    return Err(anyhow::Error::new(crate::agent::providers::LlmCallError::Server5xx {
                        provider: "anthropic".to_string(),
                        status: code,
                    }));
                }
                if let Some(ra) = retry_after {
                    anyhow::bail!("anthropic API error (retry-after: {ra}): {err_text}");
                }
                anyhow::bail!("anthropic API error: {err_text}");
            }
        };

        let mut full_content = String::new();
        let mut buffer = String::new();
        let mut thinking = ThinkingState::default();
        let mut usage_buffer = StreamingAnthropicUsage::default();

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
                    && let Ok(event) = serde_json::from_str::<serde_json::Value>(data)
                {
                    let mut pending_chunks: Vec<String> = Vec::new();
                    process_sse_event(
                        &event,
                        &mut thinking,
                        &mut usage_buffer,
                        |_| {},
                        |text| {
                            full_content.push_str(&text);
                            pending_chunks.push(text);
                        },
                    );
                    for chunk in pending_chunks {
                        chunk_tx.send(chunk).await.ok();
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
            provider = "anthropic",
            content_len = full_content.len(),
            elapsed_ms = elapsed.as_millis() as u64,
            "streaming response complete"
        );

        Ok(LlmResponse {
            content: full_content,
            tool_calls: vec![],
            usage: usage_buffer.into_token_usage(),
            finish_reason: None,
            model: Some(self.model.effective()),
            provider: Some("anthropic".to_string()),
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: thinking.blocks,
        })
    }

    fn name(&self) -> &'static str {
        "anthropic"
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

    fn supports_prefill(&self) -> bool {
        true
    }

    async fn context_limit_hint(&self, model: &str) -> Option<u32> {
        let url = format!(
            "{}/v1/models/{}",
            self.base_url.trim_end_matches('/'),
            model
        );
        let api_key = self.resolve_api_key().await;
        let mut req = self.client
            .discovery_client()
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .header("anthropic-version", "2023-06-01");
        if let Some(ref key) = api_key {
            req = if self.bearer_auth {
                req.header("authorization", format!("Bearer {key}"))
            } else {
                req.header("x-api-key", key.as_str())
            };
        }
        let resp = req.send().await.ok()?
            .error_for_status().ok()?
            .json::<serde_json::Value>().await.ok()?;

        if let Some(n) = resp.get("context_window").and_then(|v| v.as_u64()) {
            tracing::debug!(model, context_window = n, "anthropic /v1/models context_window");
            return Some(n as u32);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auth_headers_native_uses_x_api_key() {
        let secrets = Arc::new(SecretsManager::new_noop());
        let p = AnthropicProvider::for_tests("m".into(), 0.7, None, secrets);
        // for_tests defaults bearer_auth = false (native Anthropic).
        let h = p.auth_headers(Some("secret-key"));
        assert!(h.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())));
        assert!(h.contains(&("x-api-key".to_string(), "secret-key".to_string())));
        assert!(!h.iter().any(|(k, _)| k == "authorization"));
    }

    #[tokio::test]
    async fn auth_headers_bearer_for_third_party() {
        let secrets = Arc::new(SecretsManager::new_noop());
        let mut p = AnthropicProvider::for_tests("m".into(), 0.7, None, secrets);
        p.bearer_auth = true; // Kimi Code / Moonshot /anthropic gateway
        let h = p.auth_headers(Some("secret-key"));
        assert!(h.contains(&("authorization".to_string(), "Bearer secret-key".to_string())));
        assert!(!h.iter().any(|(k, _)| k == "x-api-key"));
    }

    #[tokio::test]
    async fn auth_headers_omits_credential_when_empty() {
        let secrets = Arc::new(SecretsManager::new_noop());
        let p = AnthropicProvider::for_tests("m".into(), 0.7, None, secrets);
        assert_eq!(p.auth_headers(None).len(), 1); // only anthropic-version
        assert_eq!(p.auth_headers(Some("")).len(), 1);
    }

    #[test]
    fn parse_thinking_block() {
        let json = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "let me think", "signature": "sig_xyz"},
                {"type": "text", "text": "The answer is 42."}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let resp: AnthropicResponse = serde_json::from_value(json).unwrap();
        let parsed = parse_anthropic_response(resp, "claude-opus-4-6");
        assert_eq!(parsed.content, "The answer is 42.");
        assert_eq!(parsed.thinking_blocks.len(), 1);
        assert_eq!(parsed.thinking_blocks[0].thinking, "let me think");
        assert_eq!(parsed.thinking_blocks[0].signature, "sig_xyz");
    }

    #[test]
    fn thinking_block_other_not_thinking_still_dropped() {
        let json = serde_json::json!({
            "content": [
                {"type": "unknown_future_type", "data": "x"},
                {"type": "text", "text": "hi"}
            ],
            "usage": null
        });
        let resp: AnthropicResponse = serde_json::from_value(json).unwrap();
        let parsed = parse_anthropic_response(resp, "claude-opus-4-6");
        assert_eq!(parsed.content, "hi");
        assert!(parsed.thinking_blocks.is_empty());
    }

    #[tokio::test]
    async fn build_assistant_message_with_thinking_blocks() {
        use opex_types::{Message, MessageRole, ThinkingBlock};

        let msg = Message {
            role: MessageRole::Assistant,
            content: "The answer is 42.".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![ThinkingBlock {
                thinking: "I need to reason carefully".to_string(),
                signature: "sig_abc".to_string(),
            }],
            db_id: None,
        };
        let messages = vec![msg];

        // Build using a minimal provider (no actual API call needed)
        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-opus-4-6".to_string(),
            1.0,
            Some(1024),
            secrets,
        );
        let (_, body) = provider.build_request_body(&messages, &[], CallOptions::default());
        let api_messages = body["messages"].as_array().unwrap();
        assert_eq!(api_messages.len(), 1);

        let content = api_messages[0]["content"].as_array().unwrap();
        // Thinking block must be first
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "I need to reason carefully");
        assert_eq!(content[0]["signature"], "sig_abc");
        // Text block comes after
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "The answer is 42.");
    }

    #[tokio::test]
    async fn build_assistant_message_thinking_before_tool_use() {
        use opex_types::{Message, MessageRole, ThinkingBlock, ToolCall};

        let msg = Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "my_tool".to_string(),
                arguments: serde_json::json!({"key": "value"}),
                thought_signature: None,
            }]),
            tool_call_id: None,
            thinking_blocks: vec![ThinkingBlock {
                thinking: "Should use tool".to_string(),
                signature: "sig_xyz".to_string(),
            }],
            db_id: None,
        };
        let messages = vec![msg];

        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-opus-4-6".to_string(),
            1.0,
            Some(1024),
            secrets,
        );
        let (_, body) = provider.build_request_body(&messages, &[], CallOptions::default());
        let api_messages = body["messages"].as_array().unwrap();
        let content = api_messages[0]["content"].as_array().unwrap();

        // thinking → tool_use order
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "call_1");
        assert_eq!(content[1]["name"], "my_tool");
    }

    #[test]
    fn streaming_message_start_populates_input_and_cache() {
        let lines = vec![
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":1000,"cache_creation_input_tokens":200,"cache_read_input_tokens":700,"output_tokens":1}}}"#.to_string(),
            r#"data: {"type":"message_delta","usage":{"output_tokens":500}}"#.to_string(),
            r#"data: {"type":"message_stop"}"#.to_string(),
        ];
        let usage = parse_streaming_usage_for_test(&lines);
        let u = usage.expect("usage populated");
        assert_eq!(u.input_tokens, 1000);
        // last message_delta wins (cumulative)
        assert_eq!(u.output_tokens, 500);
        assert_eq!(u.cache_creation_tokens, Some(200));
        assert_eq!(u.cache_read_tokens, Some(700));
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn streaming_no_message_start_returns_none() {
        let lines = vec![
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}"#
                .to_string(),
        ];
        let usage = parse_streaming_usage_for_test(&lines);
        assert!(usage.is_none(), "no message_start = no usage");
    }

    #[test]
    fn streaming_only_message_delta_drops_usage() {
        // Provider sends only message_delta (no message_start). Without input_tokens
        // we cannot record meaningful billing data, so we drop instead of synthesizing
        // TokenUsage{input:0, output:N} that would corrupt usage_log.
        let lines = vec![
            r#"data: {"type":"message_delta","usage":{"output_tokens":42}}"#.to_string(),
        ];
        let usage = parse_streaming_usage_for_test(&lines);
        assert!(usage.is_none(), "bare message_delta must be dropped, not corrupt billing");
    }

    #[test]
    fn streaming_multiple_message_delta_last_wins() {
        // Anthropic emits cumulative usage in each message_delta. Last value must win
        // (overwrite, not accumulate) — guards against a `=` -> `+=` regression that
        // would double-count output tokens.
        let lines = vec![
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":100,"output_tokens":1}}}"#.to_string(),
            r#"data: {"type":"message_delta","usage":{"output_tokens":150}}"#.to_string(),
            r#"data: {"type":"message_delta","usage":{"output_tokens":420}}"#.to_string(),
            r#"data: {"type":"message_stop"}"#.to_string(),
        ];
        let u = parse_streaming_usage_for_test(&lines).expect("usage populated");
        assert_eq!(u.output_tokens, 420, "last delta overwrites, never sums");
        assert_eq!(u.input_tokens, 100);
    }

    #[test]
    fn streaming_message_start_without_usage_returns_none() {
        // Anthropic sometimes omits the usage field. Setting `seen=true` outside the
        // `if let Some(usage)` guard would synthesize zeros — assert we don't.
        let lines = vec![
            r#"data: {"type":"message_start","message":{"id":"msg_x","model":"claude","content":[]}}"#.to_string(),
            r#"data: {"type":"message_stop"}"#.to_string(),
        ];
        assert!(
            parse_streaming_usage_for_test(&lines).is_none(),
            "message_start without usage must not seed an empty TokenUsage"
        );
    }

    #[test]
    fn streaming_message_start_without_cache_fields() {
        // Non-cacheable requests omit cache_*_input_tokens entirely. Ensure the
        // resulting TokenUsage carries None (not Some(0)) for both cache fields,
        // matching the non-streaming path's semantics.
        let lines = vec![
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":250,"output_tokens":1}}}"#.to_string(),
            r#"data: {"type":"message_delta","usage":{"output_tokens":80}}"#.to_string(),
            r#"data: {"type":"message_stop"}"#.to_string(),
        ];
        let u = parse_streaming_usage_for_test(&lines).expect("usage populated");
        assert_eq!(u.cache_read_tokens, None);
        assert_eq!(u.cache_creation_tokens, None);
    }

    #[test]
    fn streaming_message_delta_updates_cache_and_input_tokens() {
        // Per Anthropic spec, message_delta.usage may carry final cumulative
        // input_tokens and cache_* values updated after server-side tool use.
        // The streaming path must not lock these to message_start values only.
        let lines = vec![
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":50,"output_tokens":1}}}"#.to_string(),
            r#"data: {"type":"message_delta","usage":{"input_tokens":10000,"cache_read_input_tokens":9500,"cache_creation_input_tokens":300,"output_tokens":600}}"#.to_string(),
            r#"data: {"type":"message_stop"}"#.to_string(),
        ];
        let u = parse_streaming_usage_for_test(&lines).expect("usage populated");
        assert_eq!(u.input_tokens, 10000, "delta input_tokens must overwrite start");
        assert_eq!(u.output_tokens, 600);
        assert_eq!(u.cache_read_tokens, Some(9500), "delta cache_read must overwrite start");
        assert_eq!(u.cache_creation_tokens, Some(300));
    }

    #[test]
    fn anthropic_usage_maps_cache_fields() {
        let json = serde_json::json!({
            "content": [{"type": "text", "text": "hi"}],
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 50,
                "cache_creation_input_tokens": 200,
                "cache_read_input_tokens": 700
            }
        });
        let resp: AnthropicResponse = serde_json::from_value(json).unwrap();
        let result = parse_anthropic_response(resp, "claude-sonnet-4-6");
        let usage = result.usage.expect("usage");

        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, Some(700));
        assert_eq!(usage.cache_creation_tokens, Some(200));
        assert_eq!(usage.reasoning_tokens, None);
    }

    // ── CACHE-01 / Pitfall 1.2: tool cache breakpoint placement ──────────

    #[tokio::test]
    async fn cache_breakpoint_lands_on_last_system_tool_not_yaml() {
        // CACHE-01 / Pitfall 1.2: tools array order is system → YAML → MCP.
        // The breakpoint must land on the last SYSTEM tool, never on a YAML tool
        // (which varies per turn and would force a cache write every request).
        use opex_types::{Message, MessageRole, ToolDefinition};

        let secrets = Arc::new(SecretsManager::new_noop());
        let mut provider = AnthropicProvider::for_tests(
            "claude-sonnet-4-6".to_string(),
            0.7,
            Some(8192),
            secrets,
        );
        provider.prompt_cache = true;

        let messages = vec![Message {
            role: MessageRole::System,
            content: "you are helpful".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        // workspace_read and memory are real system tool names; my_yaml_tool is fake.
        let tools = vec![
            ToolDefinition {
                name: "workspace_read".to_string(),
                description: "".to_string(),
                input_schema: serde_json::json!({}),
            },
            ToolDefinition {
                name: "memory".to_string(),
                description: "".to_string(),
                input_schema: serde_json::json!({}),
            },
            ToolDefinition {
                name: "my_yaml_tool".to_string(),
                description: "".to_string(),
                input_schema: serde_json::json!({}),
            },
        ];

        let (_, body) = provider.build_request_body(&messages, &tools, CallOptions::default());
        let tools_json = body["tools"].as_array().expect("tools array present");

        assert_eq!(tools_json.len(), 3);
        assert!(
            tools_json[0].get("cache_control").is_none(),
            "workspace_read (index 0) must NOT have cache_control — only the LAST system tool gets it"
        );
        assert!(
            tools_json[1].get("cache_control").is_some(),
            "memory (index 1) must have cache_control — it is the last system tool in the list"
        );
        assert!(
            tools_json[2].get("cache_control").is_none(),
            "my_yaml_tool (index 2) MUST NOT have cache_control — YAML tools vary per turn (Pitfall 1.2)"
        );
    }

    #[tokio::test]
    async fn cache_breakpoint_absent_when_only_yaml_tools() {
        // No system tool in the list → no tool-level breakpoint.
        // Better to omit than to stamp a per-turn-mutable tool.
        use opex_types::{Message, MessageRole, ToolDefinition};

        let secrets = Arc::new(SecretsManager::new_noop());
        let mut provider = AnthropicProvider::for_tests(
            "claude-sonnet-4-6".to_string(),
            0.7,
            Some(8192),
            secrets,
        );
        provider.prompt_cache = true;

        let messages = vec![Message {
            role: MessageRole::System,
            content: "system".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let tools = vec![
            ToolDefinition {
                name: "yaml_only_a".to_string(),
                description: "".to_string(),
                input_schema: serde_json::json!({}),
            },
            ToolDefinition {
                name: "yaml_only_b".to_string(),
                description: "".to_string(),
                input_schema: serde_json::json!({}),
            },
        ];

        let (_, body) = provider.build_request_body(&messages, &tools, CallOptions::default());
        let tools_json = body["tools"].as_array().expect("tools array present");
        for (i, t) in tools_json.iter().enumerate() {
            assert!(
                t.get("cache_control").is_none(),
                "tool index {i} (yaml-only) must NOT have cache_control"
            );
        }
    }

    #[tokio::test]
    async fn cache_breakpoint_absent_when_prompt_cache_disabled() {
        // prompt_cache = false → no breakpoints anywhere.
        use opex_types::{Message, MessageRole, ToolDefinition};

        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-sonnet-4-6".to_string(),
            0.7,
            Some(8192),
            secrets,
        );
        // Note: for_tests sets prompt_cache: false; do not toggle it.

        let messages = vec![Message {
            role: MessageRole::System,
            content: "system".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let tools = vec![ToolDefinition {
            name: "memory".to_string(),
            description: "".to_string(),
            input_schema: serde_json::json!({}),
        }];

        let (_, body) = provider.build_request_body(&messages, &tools, CallOptions::default());

        // System should be a plain string, not a content-block array
        assert!(body["system"].is_string(), "system must be a plain string when prompt_cache=false");
        // No cache_control on any tool
        for (i, t) in body["tools"].as_array().unwrap().iter().enumerate() {
            assert!(t.get("cache_control").is_none(), "tool index {i} must have no cache_control when prompt_cache=false");
        }
    }

    // ── CACHE-02: third breakpoint — CLAUDE.md as second system content block ──

    #[tokio::test]
    async fn cache_third_breakpoint_emits_two_block_system_when_claude_md_present() {
        use opex_types::{Message, MessageRole};

        let secrets = Arc::new(SecretsManager::new_noop());
        let mut provider = AnthropicProvider::for_tests(
            "claude-sonnet-4-6".to_string(),
            0.7,
            Some(8192),
            secrets,
        );
        provider.prompt_cache = true;

        let messages = vec![Message {
            role: MessageRole::System,
            content: "you are helpful".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let opts = CallOptions {
            thinking_level: 0,
            claude_md_content: Some("# Project Rules\n- rustls only".to_string()),
            ..Default::default()
        };

        let (_, body) = provider.build_request_body(&messages, &[], opts);

        let system = body["system"].as_array().expect("system must be an array when CLAUDE.md is present");
        assert_eq!(system.len(), 2, "must have exactly 2 content blocks: system + claude_md");
        assert_eq!(system[0]["text"], "you are helpful");
        assert_eq!(system[0]["type"], "text");
        assert!(system[0].get("cache_control").is_some(), "block 0 (system) must have cache_control");
        assert!(system[1]["text"].as_str().unwrap().contains("Project Rules"));
        assert_eq!(system[1]["type"], "text");
        assert!(system[1].get("cache_control").is_some(), "block 1 (claude_md) must have cache_control");
    }

    #[tokio::test]
    async fn cache_third_breakpoint_falls_back_to_single_block_when_claude_md_absent() {
        use opex_types::{Message, MessageRole};

        let secrets = Arc::new(SecretsManager::new_noop());
        let mut provider = AnthropicProvider::for_tests(
            "claude-sonnet-4-6".to_string(),
            0.7,
            Some(8192),
            secrets,
        );
        provider.prompt_cache = true;

        let messages = vec![Message {
            role: MessageRole::System,
            content: "you are helpful".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let opts = CallOptions {
            thinking_level: 0,
            claude_md_content: None,
            ..Default::default()
        };

        let (_, body) = provider.build_request_body(&messages, &[], opts);
        let system = body["system"].as_array().expect("system must be a single-block array (Plan 01)");
        assert_eq!(system.len(), 1);
        assert!(system[0].get("cache_control").is_some());
    }

    #[tokio::test]
    async fn cache_third_breakpoint_treats_whitespace_only_claude_md_as_absent() {
        use opex_types::{Message, MessageRole};

        let secrets = Arc::new(SecretsManager::new_noop());
        let mut provider = AnthropicProvider::for_tests(
            "claude-sonnet-4-6".to_string(),
            0.7,
            Some(8192),
            secrets,
        );
        provider.prompt_cache = true;

        let messages = vec![Message {
            role: MessageRole::System,
            content: "system".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let opts = CallOptions {
            thinking_level: 0,
            claude_md_content: Some("   \n\t  \n".to_string()),
            ..Default::default()
        };

        let (_, body) = provider.build_request_body(&messages, &[], opts);
        let system = body["system"].as_array().expect("must still be an array");
        assert_eq!(system.len(), 1, "whitespace-only claude_md must be treated as absent");
    }

    #[tokio::test]
    async fn cache_third_breakpoint_disabled_when_prompt_cache_false() {
        use opex_types::{Message, MessageRole};

        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-sonnet-4-6".to_string(),
            0.7,
            Some(8192),
            secrets,
        );
        // for_tests default: prompt_cache=false

        let messages = vec![Message {
            role: MessageRole::System,
            content: "system".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let opts = CallOptions {
            thinking_level: 0,
            claude_md_content: Some("would be ignored".to_string()),
            ..Default::default()
        };

        let (_, body) = provider.build_request_body(&messages, &[], opts);
        assert!(body["system"].is_string(), "system must be a plain string when prompt_cache=false");
    }
}

