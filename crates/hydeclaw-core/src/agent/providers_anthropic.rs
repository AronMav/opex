//! Anthropic Messages API provider —
//! extracted from providers.rs for readability.

use super::{Deserialize, async_trait, Arc, SecretsManager, ModelOverride, Message, ToolDefinition, MessageRole, LlmProvider, LlmResponse, Result, mpsc, CallOptions};

// ── Extended thinking support ─────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum ThinkingMode {
    /// Opus 4.7+ and Mythos: only adaptive supported (manual → 400 error).
    AdaptiveOnly,
    /// Opus 4.6, Sonnet 4.6: adaptive recommended, manual deprecated.
    Adaptive,
    /// All others: manual budget_tokens.
    Manual,
}

fn thinking_mode(model: &str) -> ThinkingMode {
    if model.contains("claude-opus-4-7") || model.contains("claude-mythos") {
        ThinkingMode::AdaptiveOnly
    } else if model.contains("claude-opus-4-6") || model.contains("claude-sonnet-4-6") {
        ThinkingMode::Adaptive
    } else {
        ThinkingMode::Manual
    }
}

/// Returns the thinking config JSON value, or None if thinking should be disabled.
/// `effective_max_tokens` = `self.max_tokens.unwrap_or(8_192)`.
fn thinking_config(level: u8, model: &str, effective_max_tokens: u32) -> Option<serde_json::Value> {
    if level == 0 {
        return None;
    }
    match thinking_mode(model) {
        ThinkingMode::AdaptiveOnly | ThinkingMode::Adaptive => {
            let effort = match level {
                1 | 2 => "low",
                3 => "medium",
                _ => "high",
            };
            Some(serde_json::json!({
                "type": "adaptive",
                "effort": effort,
                "display": "summarized"
            }))
        }
        ThinkingMode::Manual => {
            let budget: u32 = match level {
                1 => 1_024,
                2 => 4_096,
                3 => 10_000,
                4 => 20_000,
                _ => 32_000,
            };
            let clamped = budget.min(effective_max_tokens.saturating_sub(1_000));
            if clamped < 1_024 {
                return None;
            }
            Some(serde_json::json!({
                "type": "enabled",
                "budget_tokens": clamped
            }))
        }
    }
}

// ── Anthropic Messages API Provider ──────────────────────────────────────────

pub struct AnthropicProvider {
    client: reqwest::Client,
    streaming_client: reqwest::Client,
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
        }
    }

    async fn resolve_api_key(&self) -> Option<String> {
        super::resolve_credential(
            &self.secrets,
            self.credential_scope.as_deref(),
            &self.api_key_name,
        ).await
    }

    fn build_request_body(
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
                                "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
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
        let effective_model = self.model.effective();
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
                // Anthropic cache_control requires system as array of content blocks
                body["system"] = serde_json::json!([{
                    "type": "text",
                    "text": sys,
                    "cache_control": {"type": "ephemeral"}
                }]);
            } else {
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
            // Add cache_control to last tool (Anthropic cache breakpoint rule)
            if self.prompt_cache
                && let Some(last) = tools_json.last_mut() {
                    last["cache_control"] = serde_json::json!({"type": "ephemeral"});
                }
            body["tools"] = serde_json::Value::Array(tools_json);
            // Force tool call when a skill trigger was detected in the system prompt
            if let Some(tool_name) = super::forced_skill_tool(messages, tools) {
                body["tool_choice"] = serde_json::json!({"type": "tool", "name": tool_name});
            }
        }

        (system_text, body)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicResponse {
    pub(super) content: Vec<AnthropicContentBlock>,
    pub(super) usage: Option<AnthropicUsage>,
    pub(super) stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicUsage {
    pub(super) input_tokens: u32,
    pub(super) output_tokens: u32,
    #[serde(default)]
    pub(super) cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    pub(super) cache_read_input_tokens: Option<u32>,
}

pub(super) fn parse_anthropic_response(api_resp: AnthropicResponse, model: &str) -> LlmResponse {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let mut thinking_blocks = Vec::new();

    for block in api_resp.content {
        match block {
            AnthropicContentBlock::Text { text } => {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&text);
            }
            AnthropicContentBlock::Thinking { thinking, signature } => {
                thinking_blocks.push(hydeclaw_types::ThinkingBlock { thinking, signature });
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(hydeclaw_types::ToolCall {
                    id,
                    name,
                    arguments: input,
                });
            }
            AnthropicContentBlock::Other => {}
        }
    }

    let usage = api_resp.usage.map(|u| {
        if let Some(cache_read) = u.cache_read_input_tokens {
            tracing::info!(cache_read, cache_create = u.cache_creation_input_tokens, "anthropic cache hit");
        }
        hydeclaw_types::TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        }
    });

    LlmResponse {
        content,
        tool_calls,
        usage,
        finish_reason: api_resp.stop_reason,
        model: Some(model.to_string()),
        provider: Some("anthropic".to_string()),
        fallback_notice: None,
        tools_used: vec![],
        iterations: 0,
        thinking_blocks,
    }
}

/// Process one parsed Anthropic SSE event. Calls `emit_thinking` for thinking content
/// (open/close tags + deltas) and `emit_text` for text_delta content.
fn process_sse_event(
    event: &serde_json::Value,
    thinking_content: &mut String,
    current_signature: &mut String,
    in_thinking_block: &mut bool,
    thinking_blocks: &mut Vec<hydeclaw_types::ThinkingBlock>,
    mut emit_thinking: impl FnMut(String),
    mut emit_text: impl FnMut(String),
) {
    match event.get("type").and_then(|t| t.as_str()) {
        Some("content_block_start") => {
            if event
                .get("content_block")
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("thinking")
            {
                *in_thinking_block = true;
                emit_thinking("<thinking>".to_string());
            }
        }
        Some("content_block_stop") => {
            if *in_thinking_block {
                emit_thinking("</thinking>".to_string());
                thinking_blocks.push(hydeclaw_types::ThinkingBlock {
                    thinking: std::mem::take(thinking_content),
                    signature: std::mem::take(current_signature),
                });
                *in_thinking_block = false;
            }
        }
        Some("content_block_delta") => {
            let delta = event.get("delta");
            match delta.and_then(|d| d.get("type")).and_then(|t| t.as_str()) {
                Some("text_delta") => {
                    if let Some(text) = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                        emit_text(text.to_string());
                    }
                }
                Some("thinking_delta") => {
                    if let Some(text) = delta.and_then(|d| d.get("thinking")).and_then(|t| t.as_str()) {
                        thinking_content.push_str(text);
                        emit_thinking(text.to_string());
                    }
                }
                Some("signature_delta") => {
                    if let Some(sig) = delta.and_then(|d| d.get("signature")).and_then(|s| s.as_str()) {
                        current_signature.push_str(sig);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

#[cfg(test)]
fn process_sse_events_for_test(lines: &[String]) -> (Vec<String>, Vec<hydeclaw_types::ThinkingBlock>) {
    use std::cell::RefCell;
    let chunks: RefCell<Vec<String>> = RefCell::new(vec![]);
    let mut thinking_blocks: Vec<hydeclaw_types::ThinkingBlock> = vec![];
    let mut thinking_content = String::new();
    let mut current_signature = String::new();
    let mut in_thinking_block = false;

    for line in lines {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => continue,
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        process_sse_event(
            &event,
            &mut thinking_content,
            &mut current_signature,
            &mut in_thinking_block,
            &mut thinking_blocks,
            |chunk| chunks.borrow_mut().push(chunk),
            |chunk| chunks.borrow_mut().push(chunk),
        );
    }
    (chunks.into_inner(), thinking_blocks)
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
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

        let body_text = crate::agent::providers_http::retry_http_post_custom(
            &self.client, &url, &body, "anthropic",
            crate::agent::providers_http::RETRYABLE_ANTHROPIC,
            self.max_retries,
            |req| {
                let req = req.header("anthropic-version", "2023-06-01");
                if let Some(ref key) = api_key
                    && !key.is_empty() {
                        return req.header("x-api-key", key.as_str());
                    }
                req
            },
        ).await?;

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

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
        opts: CallOptions,
    ) -> Result<LlmResponse> {
        if !tools.is_empty() {
            let response = self.chat(messages, tools, opts).await?;
            if response.tool_calls.is_empty() {
                let filtered = crate::agent::thinking::strip_thinking(&response.content);
                if !filtered.is_empty() {
                    chunk_tx.send(filtered).ok();
                }
            }
            return Ok(response);
        }

        let (_, mut body) = self.build_request_body(messages, tools, opts);
        body["stream"] = serde_json::Value::Bool(true);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        tracing::info!(provider = "anthropic", model = %self.model, "calling Anthropic API (streaming)");

        let start = std::time::Instant::now();
        let mut req = self.streaming_client
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = self.resolve_api_key().await
            && !key.is_empty()
        {
            req = req.header("x-api-key", key.as_str());
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(anyhow::Error::new(super::classify_reqwest_err(
                    e,
                    "anthropic",
                    self.timeouts.connect_secs,
                    self.timeouts.request_secs,
                )));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let code = status.as_u16();
            let retry_after = resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(std::string::ToString::to_string);
            let err_text = resp.text().await.unwrap_or_default();
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

        let mut full_content = String::new();
        let mut buffer = String::new();
        let mut thinking_content = String::new();
        let mut current_signature = String::new();
        let mut in_thinking_block = false;
        let mut thinking_blocks: Vec<hydeclaw_types::ThinkingBlock> = vec![];

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

                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                        process_sse_event(
                            &event,
                            &mut thinking_content,
                            &mut current_signature,
                            &mut in_thinking_block,
                            &mut thinking_blocks,
                            |_| {},
                            |text| {
                                full_content.push_str(&text);
                                chunk_tx.send(text).ok();
                            },
                        );
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
            usage: None,
            finish_reason: None,
            model: Some(self.model.effective()),
            provider: Some("anthropic".to_string()),
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use hydeclaw_types::{Message, MessageRole, ThinkingBlock};

        let msg = Message {
            role: MessageRole::Assistant,
            content: "The answer is 42.".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![ThinkingBlock {
                thinking: "I need to reason carefully".to_string(),
                signature: "sig_abc".to_string(),
            }],
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
        use hydeclaw_types::{Message, MessageRole, ThinkingBlock, ToolCall};

        let msg = Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                name: "my_tool".to_string(),
                arguments: serde_json::json!({"key": "value"}),
            }]),
            tool_call_id: None,
            thinking_blocks: vec![ThinkingBlock {
                thinking: "Should use tool".to_string(),
                signature: "sig_xyz".to_string(),
            }],
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
}

#[cfg(test)]
mod thinking_config_tests {
    use super::*;

    #[test]
    fn level_zero_returns_none() {
        assert!(thinking_config(0, "claude-opus-4-7", 8_192).is_none());
    }

    #[test]
    fn opus47_level1_adaptive_low() {
        let cfg = thinking_config(1, "claude-opus-4-7", 8_192).unwrap();
        assert_eq!(cfg["type"], "adaptive");
        assert_eq!(cfg["effort"], "low");
        assert_eq!(cfg["display"], "summarized");
    }

    #[test]
    fn opus47_level3_adaptive_medium() {
        let cfg = thinking_config(3, "claude-opus-4-7", 8_192).unwrap();
        assert_eq!(cfg["type"], "adaptive");
        assert_eq!(cfg["effort"], "medium");
        assert_eq!(cfg["display"], "summarized");
    }

    #[test]
    fn opus46_level5_adaptive_high() {
        let cfg = thinking_config(5, "claude-opus-4-6", 16_000).unwrap();
        assert_eq!(cfg["type"], "adaptive");
        assert_eq!(cfg["effort"], "high");
        assert_eq!(cfg["display"], "summarized");
    }

    #[test]
    fn sonnet37_level3_manual_exact_budget() {
        let cfg = thinking_config(3, "claude-sonnet-3-7", 16_000).unwrap();
        assert_eq!(cfg["type"], "enabled");
        assert_eq!(cfg["budget_tokens"], 10_000_u64);
    }

    #[test]
    fn sonnet37_level3_budget_clamped() {
        let cfg = thinking_config(3, "claude-sonnet-3-7", 8_192).unwrap();
        assert_eq!(cfg["budget_tokens"], 7_192_u64);
    }

    #[test]
    fn tight_max_tokens_returns_none() {
        assert!(thinking_config(5, "claude-haiku-4-5", 2_000).is_none());
    }

    #[test]
    fn thinking_mode_opus47_is_adaptive_only() {
        assert!(matches!(thinking_mode("claude-opus-4-7"), ThinkingMode::AdaptiveOnly));
    }

    #[test]
    fn thinking_mode_sonnet46_is_adaptive() {
        assert!(matches!(thinking_mode("claude-sonnet-4-6"), ThinkingMode::Adaptive));
    }

    #[test]
    fn thinking_mode_sonnet37_is_manual() {
        assert!(matches!(thinking_mode("claude-sonnet-3-7"), ThinkingMode::Manual));
    }

    #[test]
    fn thinking_mode_haiku45_is_manual() {
        assert!(matches!(thinking_mode("claude-haiku-4-5"), ThinkingMode::Manual));
    }

    #[tokio::test]
    async fn temperature_enforced_to_1_when_thinking_enabled() {
        use std::sync::Arc;
        let secrets = Arc::new(crate::secrets::SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-opus-4-7".to_string(),
            0.3,
            Some(16_000),
            secrets,
        );
        let opts = CallOptions { thinking_level: 3 };
        let (_, body) = provider.build_request_body(&[], &[], opts);
        let temp = body["temperature"].as_f64().expect("temperature must be in body");
        assert!(temp >= 1.0, "expected temperature >= 1.0 when thinking enabled, got {temp}");
        assert!(body.get("thinking").is_some(), "thinking field must be present");
    }

    #[tokio::test]
    async fn temperature_unchanged_when_thinking_disabled() {
        use std::sync::Arc;
        let secrets = Arc::new(crate::secrets::SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-opus-4-7".to_string(),
            0.7,
            Some(16_000),
            secrets,
        );
        let opts = CallOptions { thinking_level: 0 };
        let (_, body) = provider.build_request_body(&[], &[], opts);
        let temp = body["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < f64::EPSILON);
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn manual_thinking_config_has_no_display_field() {
        let cfg = thinking_config(3, "claude-sonnet-3-7", 16_000).unwrap();
        assert_eq!(cfg["type"], "enabled");
        assert!(cfg.get("display").is_none(), "manual config must not contain 'display' field; got: {cfg}");
    }
}

#[cfg(test)]
mod streaming_thinking_tests {
    use super::*;

    fn make_sse_line(json: &str) -> String {
        format!("data: {json}")
    }

    #[test]
    fn streaming_emits_thinking_tags_and_populates_thinking_blocks() {
        let events = vec![
            make_sse_line(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#),
            make_sse_line(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me reason..."}}"#),
            make_sse_line(r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc123"}}"#),
            make_sse_line(r#"{"type":"content_block_stop","index":0}"#),
            make_sse_line(r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#),
            make_sse_line(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Answer here."}}"#),
            make_sse_line(r#"{"type":"content_block_stop","index":1}"#),
        ];

        let (chunks, blocks) = process_sse_events_for_test(&events);

        assert!(chunks.contains(&"<thinking>".to_string()), "missing <thinking> open tag; got {chunks:?}");
        assert!(chunks.iter().any(|c| c.contains("Let me reason")), "missing thinking content; got {chunks:?}");
        assert!(chunks.contains(&"</thinking>".to_string()), "missing </thinking> close tag; got {chunks:?}");
        assert!(chunks.iter().any(|c| c.contains("Answer here")), "missing text content; got {chunks:?}");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].thinking, "Let me reason...");
        assert_eq!(blocks[0].signature, "abc123");
    }
}
