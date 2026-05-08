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
                tracing::warn!(
                    thinking_level = level,
                    model,
                    effective_max_tokens,
                    budget,
                    clamped,
                    "thinking disabled: budget after clamping is below 1024 — increase max_tokens"
                );
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

/// Buffer for streaming usage data accumulated from SSE events.
/// `message_start` populates input/cache_*. `message_delta` updates output.
#[derive(Default)]
struct StreamingAnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    /// True if any usage event (`message_start` or `message_delta`) was observed.
    /// If false at end of stream, usage stays None (no synthesized zeros).
    seen: bool,
}

impl StreamingAnthropicUsage {
    fn into_token_usage(self) -> Option<hydeclaw_types::TokenUsage> {
        if !self.seen {
            return None;
        }
        // Optional cache hit log (mirrors non-streaming path).
        if let Some(cache_read) = self.cache_read_input_tokens
            && cache_read > 0
        {
            tracing::info!(
                cache_read,
                cache_create = self.cache_creation_input_tokens,
                "anthropic streaming cache hit"
            );
        }
        Some(hydeclaw_types::TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_input_tokens,
            cache_creation_tokens: self.cache_creation_input_tokens,
            reasoning_tokens: None,
        })
    }
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
                    id: hydeclaw_types::ids::ToolCallId::from(id),
                    name,
                    arguments: input,
                });
            }
            AnthropicContentBlock::Other => {}
        }
    }

    let usage = api_resp.usage.map(|u| {
        if let Some(cache_read) = u.cache_read_input_tokens
            && cache_read > 0
        {
            tracing::info!(
                cache_read,
                cache_create = u.cache_creation_input_tokens,
                "anthropic cache hit"
            );
        }
        hydeclaw_types::TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_creation_tokens: u.cache_creation_input_tokens,
            reasoning_tokens: None,
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

/// Bundles the per-stream thinking-block state so `process_sse_event` doesn't
/// take five `&mut` accumulator parameters. Lives on the stack of the
/// streaming consume loop; reset on each new HTTP response.
#[derive(Default)]
struct ThinkingState {
    /// Accumulated text inside the current `<thinking>` block (between
    /// `content_block_start` and `content_block_stop`).
    content: String,
    /// Accumulated cryptographic signature for the current thinking block.
    signature: String,
    /// True when we are between a thinking-block start and its stop.
    in_block: bool,
    /// Sealed `ThinkingBlock` records, one per closed thinking block.
    blocks: Vec<hydeclaw_types::ThinkingBlock>,
}

/// Process one parsed Anthropic SSE event. Calls `emit_thinking` for thinking content
/// (open/close tags + deltas) and `emit_text` for text_delta content. Captures usage
/// from `message_start` (input + cache_*) and `message_delta` (cumulative output) into
/// `usage_buffer`.
fn process_sse_event(
    event: &serde_json::Value,
    thinking: &mut ThinkingState,
    usage_buffer: &mut StreamingAnthropicUsage,
    mut emit_thinking: impl FnMut(String),
    mut emit_text: impl FnMut(String),
) {
    match event.get("type").and_then(|t| t.as_str()) {
        Some("message_start") => {
            if let Some(usage) = event.get("message").and_then(|m| m.get("usage")) {
                usage_buffer.seen = true;
                if let Some(n) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.input_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.output_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_creation_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
                if let Some(n) = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_read_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
            }
        }
        Some("message_delta") => {
            // message_delta.usage carries cumulative final values per Anthropic's spec —
            // server-side tools (web search, etc.) inflate input/cache counts mid-stream,
            // so we overwrite each field present rather than relying on message_start alone.
            // Without message_start observed first, we drop the data: a bare message_delta
            // would record TokenUsage{input:0, output:N} into usage_log, corrupting billing.
            if let Some(usage) = event.get("usage") {
                if !usage_buffer.seen {
                    tracing::warn!("anthropic message_delta without preceding message_start — dropping usage");
                    return;
                }
                if let Some(n) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.output_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    usage_buffer.input_tokens = n.min(u32::MAX as u64) as u32;
                }
                if let Some(n) = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_creation_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
                if let Some(n) = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage_buffer.cache_read_input_tokens = Some(n.min(u32::MAX as u64) as u32);
                }
            }
        }
        Some("content_block_start")
            if event
                .get("content_block")
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("thinking") =>
        {
            thinking.in_block = true;
            emit_thinking("<thinking>".to_string());
        }
        Some("content_block_stop") if thinking.in_block => {
            emit_thinking("</thinking>".to_string());
            thinking.blocks.push(hydeclaw_types::ThinkingBlock {
                thinking: std::mem::take(&mut thinking.content),
                signature: std::mem::take(&mut thinking.signature),
            });
            thinking.in_block = false;
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
                        thinking.content.push_str(text);
                        emit_thinking(text.to_string());
                    }
                }
                Some("signature_delta") => {
                    if let Some(sig) = delta.and_then(|d| d.get("signature")).and_then(|s| s.as_str()) {
                        thinking.signature.push_str(sig);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Test helper that mirrors production behavior: emit_thinking is discarded (as in chat_stream),
/// emit_text goes to text_chunks. Returns (text_chunks, thinking_chunks, blocks) where
/// thinking_chunks captures what emit_thinking would have sent (for assertion purposes only).
#[cfg(test)]
fn process_sse_events_for_test(
    lines: &[String],
) -> (Vec<String>, Vec<String>, Vec<hydeclaw_types::ThinkingBlock>) {
    use std::cell::RefCell;
    let text_chunks: RefCell<Vec<String>> = RefCell::new(vec![]);
    let thinking_chunks: RefCell<Vec<String>> = RefCell::new(vec![]);
    let mut thinking = ThinkingState::default();
    let mut usage_buffer = StreamingAnthropicUsage::default();

    for line in lines {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => continue,
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        process_sse_event(
            &event,
            &mut thinking,
            &mut usage_buffer,
            |chunk| thinking_chunks.borrow_mut().push(chunk),
            |chunk| text_chunks.borrow_mut().push(chunk),
        );
    }
    (text_chunks.into_inner(), thinking_chunks.into_inner(), thinking.blocks)
}

#[cfg(test)]
fn parse_streaming_usage_for_test(lines: &[String]) -> Option<hydeclaw_types::TokenUsage> {
    let mut thinking = ThinkingState::default();
    let mut usage_buffer = StreamingAnthropicUsage::default();

    for line in lines {
        let data = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => continue,
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        process_sse_event(
            &event,
            &mut thinking,
            &mut usage_buffer,
            |_| {},
            |_| {},
        );
    }

    usage_buffer.into_token_usage()
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

        let body_text = crate::agent::providers::http::retry_http_post_custom(
            &self.client, &url, &body, "anthropic",
            crate::agent::providers::http::RETRYABLE_ANTHROPIC,
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
                    process_sse_event(
                        &event,
                        &mut thinking,
                        &mut usage_buffer,
                        |_| {},
                        |text| {
                            full_content.push_str(&text);
                            chunk_tx.send(text).ok();
                        },
                    );
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
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .header("anthropic-version", "2023-06-01");
        if let Some(ref key) = api_key {
            req = req.header("x-api-key", key.as_str());
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
        use hydeclaw_types::{Message, MessageRole, ThinkingBlock, ToolCall};

        let msg = Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "my_tool".to_string(),
                arguments: serde_json::json!({"key": "value"}),
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
        use hydeclaw_types::{Message, MessageRole, ToolDefinition};

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
        use hydeclaw_types::{Message, MessageRole, ToolDefinition};

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
        use hydeclaw_types::{Message, MessageRole, ToolDefinition};

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
        use hydeclaw_types::{Message, MessageRole};

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
        use hydeclaw_types::{Message, MessageRole};

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
        };

        let (_, body) = provider.build_request_body(&messages, &[], opts);
        let system = body["system"].as_array().expect("system must be a single-block array (Plan 01)");
        assert_eq!(system.len(), 1);
        assert!(system[0].get("cache_control").is_some());
    }

    #[tokio::test]
    async fn cache_third_breakpoint_treats_whitespace_only_claude_md_as_absent() {
        use hydeclaw_types::{Message, MessageRole};

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
        };

        let (_, body) = provider.build_request_body(&messages, &[], opts);
        let system = body["system"].as_array().expect("must still be an array");
        assert_eq!(system.len(), 1, "whitespace-only claude_md must be treated as absent");
    }

    #[tokio::test]
    async fn cache_third_breakpoint_disabled_when_prompt_cache_false() {
        use hydeclaw_types::{Message, MessageRole};

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
        };

        let (_, body) = provider.build_request_body(&messages, &[], opts);
        assert!(body["system"].is_string(), "system must be a plain string when prompt_cache=false");
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
        let opts = CallOptions { thinking_level: 3, ..Default::default() };
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
        let opts = CallOptions { thinking_level: 0, ..Default::default() };
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

        let (text_chunks, thinking_chunks, blocks) = process_sse_events_for_test(&events);

        // Text stream (what the UI receives): only actual text, no thinking fragments
        assert!(
            text_chunks.iter().any(|c| c.contains("Answer here")),
            "text stream missing answer; got {text_chunks:?}"
        );
        assert!(
            !text_chunks.iter().any(|c| c.contains("thinking")),
            "text stream must not contain thinking fragments; got {text_chunks:?}"
        );

        // Thinking stream (discarded in production, collected here for assertion)
        assert!(
            thinking_chunks.contains(&"<thinking>".to_string()),
            "missing <thinking> open tag; got {thinking_chunks:?}"
        );
        assert!(
            thinking_chunks.iter().any(|c| c.contains("Let me reason")),
            "missing thinking content; got {thinking_chunks:?}"
        );
        assert!(
            thinking_chunks.contains(&"</thinking>".to_string()),
            "missing </thinking> close tag; got {thinking_chunks:?}"
        );

        // Structured thinking blocks
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].thinking, "Let me reason...");
        assert_eq!(blocks[0].signature, "abc123");
    }
}
