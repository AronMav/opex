//! OpenAI-compatible LLM provider (`MiniMax`, `OpenAI`, Ollama, etc.) —
//! extracted from providers.rs for readability.

use super::{async_trait, Deserialize, Arc, SecretsManager, ModelOverride, LlmProvider, Message, ToolDefinition, Result, LlmResponse, messages_to_openai_format, mpsc};
use crate::agent::providers_http::SendError;

// ── OpenAI-Compatible Provider (works with MiniMax, OpenAI, Ollama, etc.) ──

pub struct OpenAiCompatibleProvider {
    provider_name: String,
    client: reqwest::Client,
    streaming_client: reqwest::Client,
    url: String,
    /// Optional secret name for dynamic base URL resolution (e.g. "`OLLAMA_URL`").
    /// When set, the URL is resolved from secrets on each call, overriding `url`.
    base_url_env: Option<String>,
    /// URL path suffix appended to `base_url` when resolving dynamically.
    url_suffix: String,
    api_key_name: String,
    /// Optional list of API key env names for round-robin rotation.
    api_key_names: Vec<String>,
    /// Atomic counter for round-robin key selection.
    key_counter: std::sync::atomic::AtomicUsize,
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
}

impl OpenAiCompatibleProvider {
    /// Build an `OpenAiCompatibleProvider` from a `ProviderRow`, storing the
    /// shared `cancel` token + typed `timeouts` so `chat_stream` can thread
    /// them into `stream_with_cancellation`.
    ///
    /// HTTP clients are built via `build_provider_clients(&timeouts)` honoring
    /// `connect_secs` / `request_secs` from the config (not the legacy
    /// 10s/120s hardcoded values).
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
        let meta = super::PROVIDER_TYPES
            .iter()
            .find(|pt| pt.id == row.provider_type);
        let default_base = meta.map_or("", |pt| pt.default_base_url);
        let key_env = meta.map_or("API_KEY", |pt| pt.default_secret_name);

        let base = row
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_base.to_string());
        let url = super::resolve_chat_url(&row.provider_type, &base);
        let model = overrides
            .model
            .clone()
            .unwrap_or_else(|| row.default_model.clone().unwrap_or_default());

        let credential_scope = row.id.to_string();

        // Build HTTP clients with configured timeouts (not legacy hardcoded 10s/120s).
        let (client, streaming_client) = super::build_provider_clients(&timeouts);

        let temperature = overrides.temperature.unwrap_or(0.7);
        let max_tokens = overrides.max_tokens;

        let mut provider = Self {
            provider_name: row.provider_type.clone(),
            client,
            streaming_client,
            url,
            base_url_env: None,
            url_suffix: String::new(),
            api_key_name: key_env.to_string(),
            api_key_names: Vec::new(),
            key_counter: std::sync::atomic::AtomicUsize::new(0),
            credential_scope: Some(credential_scope),
            secrets,
            model: super::ModelOverride::new(model),
            temperature,
            max_tokens,
            timeouts,
            cancel,
        };

        if !opts.api_key_envs.is_empty() {
            provider = provider.with_keys(opts.api_key_envs);
        }
        Ok(provider)
    }

    /// Set vault credential scope (provider UUID) for `LLM_CREDENTIALS` lookup.
    ///
    /// `new_from_row` builds the scope literally from the row UUID.
    /// Kept public for external consumers as a stable fluent API.
    #[allow(dead_code)]
    pub fn with_credential_scope(mut self, scope: String) -> Self {
        self.credential_scope = Some(scope);
        self
    }

    #[cfg(test)]
    pub(crate) fn test_temperature(&self) -> f64 {
        self.temperature
    }

    #[cfg(test)]
    pub(crate) fn test_max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }

    #[cfg(test)]
    pub(crate) fn test_timeouts(&self) -> super::TimeoutsConfig {
        self.timeouts
    }

    /// Create with round-robin API key rotation.
    pub fn with_keys(mut self, api_key_names: Vec<String>) -> Self {
        self.api_key_names = api_key_names;
        self
    }

    /// Set dynamic base URL resolution from secrets (e.g. "`OLLAMA_URL`").
    /// On each LLM call, resolves the secret and appends `suffix` to form the full URL.
    /// Kept for future secret-backed URL resolution; no longer called after Task 12
    /// consolidated provider construction via `build_provider` + `ProviderRow.base_url`.
    #[allow(dead_code)]
    pub fn with_base_url_env(mut self, env_name: &str, suffix: &str) -> Self {
        self.base_url_env = Some(env_name.to_string());
        self.url_suffix = suffix.to_string();
        self
    }

    /// Resolve the effective URL: dynamic from secrets or static.
    async fn resolve_url(&self) -> String {
        if let Some(ref env_name) = self.base_url_env
            && let Some(base) = self.secrets.get_scoped(env_name, "").await {
                let base = base.trim_end_matches('/');
                return format!("{}{}", base, self.url_suffix);
            }
        self.url.clone()
    }

    /// Whether this provider supports `parallel_tool_calls` parameter.
    fn supports_parallel_tools(&self) -> bool {
        matches!(self.provider_name.as_str(), "openai" | "ollama")
    }

    /// Resolve the current API key: vault-scoped → round-robin → legacy secret name → env.
    async fn resolve_api_key(&self) -> String {
        // Vault-scoped credential (provider UUID)
        if let Some(val) = super::resolve_credential(
            &self.secrets,
            self.credential_scope.as_deref(),
            "",
        ).await {
            return val;
        }
        // Round-robin keys
        if !self.api_key_names.is_empty() {
            let idx = self.key_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                % self.api_key_names.len();
            let key_name = &self.api_key_names[idx];
            if let Some(val) = self.secrets.get(key_name).await {
                return val;
            }
            std::env::var(key_name).unwrap_or_default()
        } else if !self.api_key_name.is_empty() {
            if let Some(val) = self.secrets.get(&self.api_key_name).await {
                return val;
            }
            std::env::var(&self.api_key_name).unwrap_or_default()
        } else {
            String::new()
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let effective_model = self.model.effective();
        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": messages_to_openai_format(messages),
            "temperature": self.temperature,
        });
        if let Some(mt) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }

        if !tools.is_empty() {
            let tools_json: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools_json);
            if let Some(tool_name) = super::forced_skill_tool(messages, tools) {
                // Force a specific tool — parallel_tool_calls is incompatible with forced tool_choice
                body["tool_choice"] = serde_json::json!({
                    "type": "function",
                    "function": {"name": tool_name}
                });
            } else if self.supports_parallel_tools() {
                body["parallel_tool_calls"] = serde_json::json!(true);
            }
        }

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
        let ctx_chars: usize = messages.iter().map(|m| {
            m.content.len()
                + m.tool_calls.as_deref().unwrap_or(&[]).iter()
                    .map(|tc| tc.arguments.to_string().len())
                    .sum::<usize>()
        }).sum();
        if ctx_chars > LARGE_CONTEXT_CHARS {
            tracing::warn!(
                provider = %self.provider_name,
                model = %self.model,
                context_chars = ctx_chars,
                threshold = LARGE_CONTEXT_CHARS,
                "large context being sent to LLM — provider may reject with 5xx or truncate silently"
            );
        }

        let api_key = self.resolve_api_key().await;
        let effective_url = self.resolve_url().await;
        let body_text = crate::agent::providers_http::retry_http_post(
            &self.client,
            &effective_url,
            &body,
            &api_key,
            &self.provider_name,
            crate::agent::providers_http::RETRYABLE_OPENAI,
        ).await?;
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

                let mut req = self.client.post(&effective_url).json(&body);
                if !retry_key.is_empty() {
                    req = req.bearer_auth(&retry_key);
                }
                match req.send().await {
                    Ok(resp) => {
                        if let Ok(text) = resp.text().await {
                            if let Ok(parsed) = serde_json::from_str::<ChatCompletionResponse>(&text)
                                && let Some(c) = parsed.choices.into_iter().next() {
                                    found = Some(c);
                                    break;
                                }
                            last_err = format!("empty choices (attempt {})", attempt + 1);
                        }
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
                hydeclaw_types::ToolCall {
                    id: tc.id,
                    name: tool_name,
                    arguments,
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

        let usage = api_resp.usage.map(|u| hydeclaw_types::TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
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

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
    ) -> Result<LlmResponse> {
        let effective_model = self.model.effective();
        let mut body = serde_json::json!({
            "model": effective_model,
            "messages": messages_to_openai_format(messages),
            "temperature": self.temperature,
            "stream": true,
            // Opt into the usage block on the final chunk. OpenAI-compatible
            // servers (Ollama, vLLM, SGLang, LiteLLM, DeepSeek, Moonshot, …)
            // omit `usage` from streaming responses by default; without this
            // flag we record 0 input/output tokens for every message on
            // locally-hosted backends. The parser already reads usage when
            // present, so this request-side opt-in is all that's needed.
            "stream_options": { "include_usage": true },
        });
        if let Some(mt) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if !tools.is_empty() {
            let tools_json: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                }))
                .collect();
            body["tools"] = serde_json::Value::Array(tools_json);
            if let Some(tool_name) = super::forced_skill_tool(messages, tools) {
                body["tool_choice"] = serde_json::json!({
                    "type": "function",
                    "function": {"name": tool_name}
                });
            } else if self.supports_parallel_tools() {
                body["parallel_tool_calls"] = serde_json::json!(true);
            }
        }

        tracing::info!(
            provider = %self.provider_name,
            model = %self.model,
            messages = messages.len(),
            tools = tools.len(),
            "calling LLM API (streaming)"
        );

        const LARGE_CONTEXT_CHARS: usize = 200_000;
        let ctx_chars: usize = messages.iter().map(|m| {
            m.content.len()
                + m.tool_calls.as_deref().unwrap_or(&[]).iter()
                    .map(|tc| tc.arguments.to_string().len())
                    .sum::<usize>()
        }).sum();
        if ctx_chars > LARGE_CONTEXT_CHARS {
            tracing::warn!(
                provider = %self.provider_name,
                model = %self.model,
                context_chars = ctx_chars,
                threshold = LARGE_CONTEXT_CHARS,
                "large context being sent to LLM — provider may reject with 5xx or truncate silently"
            );
        }

        let start = std::time::Instant::now();
        let api_key = self.resolve_api_key().await;
        let effective_url = self.resolve_url().await;
        let api_key_clone = api_key.clone();
        let resp = crate::agent::providers_http::send_with_retry(
            &self.streaming_client,
            &effective_url,
            &body,
            &self.provider_name,
            crate::agent::providers_http::RETRYABLE_OPENAI,
            move |req| if api_key_clone.is_empty() { req } else { req.bearer_auth(&api_key_clone) },
        )
        .await
        .map_err(|e| match e {
            SendError::Http { status, .. } if status == 401 || status == 403 =>
                anyhow::Error::new(LlmCallError::AuthError {
                    provider: self.provider_name.clone(),
                    status,
                }),
            SendError::Http { status, .. } =>
                anyhow::Error::new(LlmCallError::Server5xx {
                    provider: self.provider_name.clone(),
                    status,
                }),
            SendError::Network(e) =>
                anyhow::Error::new(super::classify_reqwest_err(
                    e,
                    &self.provider_name,
                    self.timeouts.connect_secs,
                    self.timeouts.request_secs,
                )),
        })?;

        // Parse SSE stream: accumulate content (streamed) + tool calls (buffered)
        let mut full_content = String::new();
        let mut buffer = String::new();
        let mut thinking_filter = crate::agent::thinking::ThinkingFilter::new();
        // Indexed by tool_call index: (id, name, arguments)
        let mut tool_call_parts: Vec<(String, String, String)> = Vec::new();
        let mut usage: Option<(u32, u32)> = None;
        let mut finish_reason: Option<String> = None;

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
        'outer: loop {
            let chunk_result = match StreamExt::next(&mut byte_stream).await {
                Some(r) => r,
                None => break 'outer, // stream ended (either clean EOF or cancelled — slot tells us which)
            };
            let chunk_bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    // Bubble as anyhow wrapping typed Network variant — Task 17 routing
                    // can downcast to LlmCallError and decide failover.
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
                    if data.trim() == "[DONE]" {
                        break 'outer;
                    }

                    match serde_json::from_str::<StreamChunk>(data) {
                        Ok(chunk_json) => {
                            // Capture usage if present (some providers send in final chunk)
                            if let Some(ref u) = chunk_json.usage {
                                usage = Some((u.prompt_tokens, u.completion_tokens));
                            }
                            if let Some(choice) = chunk_json.choices.first() {
                                // Capture finish reason
                                if let Some(ref fr) = choice.finish_reason {
                                    finish_reason = Some(fr.clone());
                                }
                                // Stream content tokens
                                if let Some(ref content) = choice.delta.content {
                                    full_content.push_str(content);
                                    let filtered = thinking_filter.process(content);
                                    if !filtered.is_empty() {
                                        chunk_tx.send(filtered).ok();
                                    }
                                }
                                // Accumulate tool call deltas by index
                                for tc in &choice.delta.tool_calls {
                                    let idx = tc.index;
                                    while tool_call_parts.len() <= idx {
                                        tool_call_parts.push((String::new(), String::new(), String::new()));
                                    }
                                    if let Some(ref id) = tc.id {
                                        tool_call_parts[idx].0 = id.clone();
                                    }
                                    if let Some(ref func) = tc.function {
                                        if let Some(ref name) = func.name {
                                            // Replace, don't append — name arrives once or repeated,
                                            // unlike arguments which stream incrementally.
                                            tool_call_parts[idx].1 = name.clone();
                                        }
                                        if let Some(ref args) = func.arguments {
                                            tool_call_parts[idx].2.push_str(args);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // Issue C (SchemaError): on the FIRST parse failure
                            // before any text has been streamed, surface a typed
                            // pre-stream SchemaError so RoutingProvider can fail
                            // over (at_bytes=0 ⇒ failover-worthy per spec §4.4).
                            // Once content has already streamed, continue-and-skip
                            // to preserve the in-progress response (subsequent
                            // noise like heartbeats also lands here harmlessly).
                            if full_content.is_empty() {
                                tracing::warn!(
                                    provider = %self.provider_name,
                                    error = %e,
                                    "SSE parse failed pre-stream, classifying as SchemaError"
                                );
                                return Err(anyhow::Error::new(LlmCallError::SchemaError {
                                    provider: self.provider_name.clone(),
                                    detail: e.to_string(),
                                    at_bytes: 0,
                                }));
                            }
                            tracing::debug!(
                                provider = %self.provider_name,
                                error = %e,
                                "failed to parse SSE chunk mid-stream, skipping"
                            );
                            continue;
                        }
                    }
                }
            }
        }

        // Stream exited. If cancellation fired, surface the typed reason with
        // the partial text we already streamed — callers can downcast to
        // `LlmCallError` and either persist a partial assistant turn
        // (user_cancelled / shutdown_drain / max_duration / inactivity) or
        // treat it as failover-worthy (see `LlmCallError::is_failover_worthy`).
        if let Some(reason) = slot.get() {
            use crate::agent::providers::error::{CancelReason, PartialState};
            let partial_state = if !tool_call_parts.is_empty() {
                PartialState::ToolUse
            } else if !full_content.is_empty() {
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
        // Convert accumulated tool call parts to ToolCall values
        let mut tool_calls: Vec<hydeclaw_types::ToolCall> = tool_call_parts
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| {
                let arguments = match crate::agent::json_repair::repair_json(&args) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(tool = %name, error = %e, raw_len = args.len(), "tool call JSON repair failed, using empty args");
                        serde_json::Value::Object(Default::default())
                    }
                };
                hydeclaw_types::ToolCall { id, name, arguments }
            })
            .collect();

        // Extract any MiniMax XML tool calls that leaked into the streamed text.
        let (full_content, xml_calls) = extract_minimax_xml_tool_calls(&full_content);
        if !xml_calls.is_empty() {
            tracing::warn!(
                provider = %self.provider_name,
                count = xml_calls.len(),
                "extracted MiniMax XML tool calls from streaming content"
            );
            tool_calls.extend(xml_calls);
        }

        tracing::info!(
            provider = %self.provider_name,
            content_len = full_content.len(),
            tool_calls = tool_calls.len(),
            finish_reason = ?finish_reason,
            elapsed_ms = elapsed.as_millis() as u64,
            "streaming response complete"
        );

        Ok(LlmResponse {
            content: full_content,
            tool_calls,
            usage: usage.map(|(inp, out)| hydeclaw_types::TokenUsage {
                input_tokens: inp,
                output_tokens: out,
            }),
            model: Some(effective_model),
            provider: Some(self.provider_name.clone()),
            fallback_notice: None,
            finish_reason,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        })
    }

    fn name(&self) -> &str {
        &self.provider_name
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
}

// ── OpenAI-compatible API response types ──

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

fn deserialize_null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(std::option::Option::unwrap_or_default)
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChatToolCall {
    id: String,
    function: ChatFunction,
}

#[derive(Debug, Deserialize)]
struct ChatFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// ── MiniMax XML tool call extraction ──

/// Extract `<minimax:tool_call>` XML blocks from LLM response content.
///
/// `MiniMax` sometimes leaks its internal XML tool-calling format in the text body
/// instead of using the standard `tool_calls` JSON array. This function parses
/// those blocks, strips them from the visible content, and returns them as proper
/// [`hydeclaw_types::ToolCall`] values so the engine can execute them normally.
///
/// Format emitted by `MiniMax`:
/// ```xml
/// <minimax:tool_call>
/// <invoke name="tool_name">
/// <parameter name="param1">value</parameter>
/// </invoke>
/// </minimax:tool_call>
/// ```
pub(crate) fn extract_minimax_xml_tool_calls(
    content: &str,
) -> (String, Vec<hydeclaw_types::ToolCall>) {
    const OPEN: &str = "<minimax:tool_call>";
    const CLOSE: &str = "</minimax:tool_call>";

    if !content.contains(OPEN) {
        return (content.to_string(), vec![]);
    }

    let mut tool_calls: Vec<hydeclaw_types::ToolCall> = Vec::new();
    let mut cleaned = String::new();
    let mut rest = content;

    loop {
        match rest.find(OPEN) {
            None => {
                cleaned.push_str(rest);
                break;
            }
            Some(start) => {
                cleaned.push_str(&rest[..start]);
                let after_open = &rest[start + OPEN.len()..];
                match after_open.find(CLOSE) {
                    None => break, // unclosed block — discard rest
                    Some(close_pos) => {
                        let block = &after_open[..close_pos];
                        rest = &after_open[close_pos + CLOSE.len()..];
                        parse_xml_invoke_blocks(block, &mut tool_calls);
                    }
                }
            }
        }
    }

    (cleaned.trim().to_string(), tool_calls)
}

/// Parse `<invoke name="...">...</invoke>` elements and push them into `out`.
fn parse_xml_invoke_blocks(block: &str, out: &mut Vec<hydeclaw_types::ToolCall>) {
    const INV_OPEN: &str = "<invoke";
    const INV_CLOSE: &str = "</invoke>";

    let mut rest = block;
    while let Some(start) = rest.find(INV_OPEN) {
        let after_tag = &rest[start + INV_OPEN.len()..];

        let Some(name) = xml_extract_attr(after_tag, "name") else { break };

        // Skip to end of opening tag (`>`)
        let Some(gt) = after_tag.find('>') else { break };
        let body_and_rest = &after_tag[gt + 1..];

        let Some(close_pos) = body_and_rest.find(INV_CLOSE) else { break };
        let invoke_body = &body_and_rest[..close_pos];
        rest = &body_and_rest[close_pos + INV_CLOSE.len()..];

        let mut args = serde_json::Map::new();
        parse_xml_parameters(invoke_body, &mut args);

        out.push(hydeclaw_types::ToolCall {
            id: format!("xml-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            name,
            arguments: serde_json::Value::Object(args),
        });
    }
}

/// Parse `<parameter name="...">VALUE</parameter>` pairs into a JSON map.
fn parse_xml_parameters(body: &str, out: &mut serde_json::Map<String, serde_json::Value>) {
    const PARAM_OPEN: &str = "<parameter";
    const PARAM_CLOSE: &str = "</parameter>";

    let mut rest = body;
    while let Some(start) = rest.find(PARAM_OPEN) {
        let after_tag = &rest[start + PARAM_OPEN.len()..];

        let Some(name) = xml_extract_attr(after_tag, "name") else { break };

        let Some(gt) = after_tag.find('>') else { break };
        let val_and_rest = &after_tag[gt + 1..];

        let Some(close_pos) = val_and_rest.find(PARAM_CLOSE) else { break };
        let raw_val = val_and_rest[..close_pos].trim();
        rest = &val_and_rest[close_pos + PARAM_CLOSE.len()..];

        // Coerce numeric/bool values; everything else is a string.
        let json_val = if let Ok(n) = raw_val.parse::<i64>() {
            serde_json::Value::Number(n.into())
        } else if raw_val == "true" {
            serde_json::Value::Bool(true)
        } else if raw_val == "false" {
            serde_json::Value::Bool(false)
        } else {
            serde_json::Value::String(raw_val.to_string())
        };

        out.insert(name, json_val);
    }
}

/// Extract `attr="VALUE"` from an XML tag fragment (everything after the tag name).
fn xml_extract_attr(s: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = s.find(needle.as_str())? + needle.len();
    let end = s[start..].find('"')?;
    Some(s[start..start + end].to_string())
}

#[cfg(test)]
mod xml_tests {
    use super::*;

    #[test]
    fn test_extract_single_invoke() {
        let input = "Some text\n<minimax:tool_call>\n<invoke name=\"brave_search\">\n<parameter name=\"q\">test query</parameter>\n<parameter name=\"count\">5</parameter>\n</invoke>\n</minimax:tool_call>\nMore text";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        // Double newline is expected: text before ends with \n, text after starts with \n
        assert_eq!(content, "Some text\n\nMore text");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "brave_search");
        assert_eq!(calls[0].arguments["q"], "test query");
        assert_eq!(calls[0].arguments["count"], 5);
    }

    #[test]
    fn test_extract_multiple_invokes_in_one_block() {
        let input = "<minimax:tool_call><invoke name=\"search\">\n<parameter name=\"q\">foo</parameter>\n</invoke>\n<invoke name=\"web\">\n<parameter name=\"url\">https://example.com</parameter>\n</invoke>\n</minimax:tool_call>";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(content, "");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[1].name, "web");
    }

    #[test]
    fn test_extract_multiple_blocks() {
        let input = "<minimax:tool_call><invoke name=\"a\"><parameter name=\"x\">1</parameter></invoke></minimax:tool_call>\n<minimax:tool_call><invoke name=\"b\"><parameter name=\"y\">2</parameter></invoke></minimax:tool_call>";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(content, "");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn test_no_xml_passthrough() {
        let input = "Normal response with no XML";
        let (content, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(content, input);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_boolean_and_numeric_coercion() {
        let input = "<minimax:tool_call><invoke name=\"t\"><parameter name=\"n\">42</parameter><parameter name=\"b\">true</parameter><parameter name=\"s\">hello</parameter></invoke></minimax:tool_call>";
        let (_, calls) = extract_minimax_xml_tool_calls(input);
        assert_eq!(calls[0].arguments["n"], 42);
        assert_eq!(calls[0].arguments["b"], true);
        assert_eq!(calls[0].arguments["s"], "hello");
    }
}

// ── SSE streaming types ──

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<StreamFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct StreamFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}
