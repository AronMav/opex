//! Provider abstraction layer: the [`LlmProvider`] trait, the
//! [`UnconfiguredProvider`] sentinel, [`ModelOverride`], and a handful of
//! cross-cutting helpers shared by every concrete impl
//! (`build_provider_clients`, `resolve_credential`, `forced_skill_tool`,
//! `messages_to_openai_format`, `strip_orphaned_tool_messages`).
//!
//! Concrete provider implementations live as plain submodules of this
//! module (`openai`, `anthropic`, `google`, `claude_cli`). Their `super::`
//! paths resolve to `agent::providers::*`, so anything they consume must
//! be re-exported in this `mod.rs` or addressed via the explicit
//! `super::sub_module::*` path.
//!
//! Sub-modules:
//! - [`registry`] — `ProviderTypeMeta` + the static `PROVIDER_TYPES` table
//! - [`factory`] — `build_provider`, `build_cli_provider`, `resolve_*`
//! - [`routing`] — `RoutingProvider` + condition dispatch + failover
//! - [`timeouts`], [`error`], [`cancellable_stream`] — leaves used by
//!   every provider impl

use anyhow::Result;
use async_trait::async_trait;
use opex_types::{LlmResponse, Message, MessageRole, ToolDefinition};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::secrets::SecretsManager;

// Feature-gated provider implementations.
#[cfg(feature = "gemini-cloudcode")]
pub mod gemini_cloudcode;
#[cfg(feature = "gemini-cloudcode")]
pub(crate) use gemini_cloudcode::GeminiCloudCodeProvider;

// Concrete provider implementations.
mod openai;
pub(crate) use openai::OpenAiCompatibleProvider;
mod anthropic;
pub(crate) use anthropic::AnthropicProvider;
#[cfg(test)]
use anthropic::{AnthropicContentBlock, AnthropicResponse, AnthropicUsage, parse_anthropic_response};
mod google;
pub(crate) use google::GoogleProvider;
#[cfg(test)]
use google::messages_to_gemini_format;
mod claude_cli;
pub(crate) use claude_cli::ClaudeCliProvider;

// Shared HTTP retry/backoff helpers used by openai/anthropic/google impls.
pub(crate) mod http;

pub mod timeouts;
pub use timeouts::TimeoutsConfig;

pub mod error;
#[allow(unused_imports)]
pub use error::{LlmCallError, CancelReason, classify_reqwest_err};

#[cfg(test)]
mod routing_tests;

#[cfg(test)]
mod build_provider_tests;

pub mod cancellable_stream;
#[allow(unused_imports)]
pub use cancellable_stream::{CancelSlot, set_and_cancel};

mod factory;
pub use factory::{
    CliContext, ProviderOverrides, build_cli_provider, build_provider,
    resolve_provider_for_agent,
};
// Public API surface preserved from the pre-split monolith — has no
// in-tree caller today but is part of the published namespace.
#[allow(unused_imports)]
pub use factory::create_cli_provider_with_options;

mod registry;
pub use registry::resolve_chat_url;
// Same as above: kept on the public surface for stability.
#[allow(unused_imports)]
pub use registry::{ProviderTypeMeta, default_base_url_for_type};
pub(crate) use registry::PROVIDER_TYPES;

mod routing;
pub use routing::create_routing_provider;
#[allow(unused_imports)]
pub use routing::RoutingProvider;

pub mod catalog;

// ── UnconfiguredProvider sentinel ─────────────────────────────────────────────

/// Sentinel "unconfigured" provider used when no usable LLM backend could be
/// built for an agent (missing `connection`, missing DB row, `build_provider`
/// failure, etc.).
///
/// It implements `LlmProvider` and returns a classified `LlmCallError::AuthError`
/// on every `chat()` / `chat_stream()` invocation, which is the closest
/// semantic match in our typed error enum ("provider is not usable — don't
/// fail over, don't retry; surface to the user"). `AuthError` is
/// non-failover-worthy, so `RoutingProvider::handle_provider_error` bubbles
/// it up with a cooldown floor, preserving a consistent runtime behavior
/// regardless of how the misconfiguration happened.
///
/// Use `UnconfiguredProvider::new(reason)` so the error carries a
/// human-readable hint (displayed in logs and — via `abort_reason` — to
/// operators). The `reason` is rendered into the provider name so it shows
/// up in structured logs and error formatting.
pub(crate) struct UnconfiguredProvider {
    reason: String,
}

impl UnconfiguredProvider {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }

    fn err(&self) -> anyhow::Error {
        anyhow::Error::new(LlmCallError::AuthError {
            provider: format!("unconfigured ({})", self.reason),
            // 503 mirrors HTTP semantics: "service unavailable / not yet
            // configured". AuthError is the non-failover-worthy carrier;
            // the status is advisory.
            status: 503,
        })
    }
}

#[async_trait]
impl LlmProvider for UnconfiguredProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _opts: CallOptions,
    ) -> Result<LlmResponse> {
        Err(self.err())
    }

    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _chunk_tx: mpsc::Sender<String>,
        _opts: CallOptions,
    ) -> Result<LlmResponse> {
        Err(self.err())
    }

    fn name(&self) -> &str {
        "unconfigured"
    }

    fn current_model(&self) -> String {
        "unconfigured".to_string()
    }
}

/// Per-call LLM options. Passed through the entire call chain from execute.rs
/// to the provider. All providers except AnthropicProvider ignore this.
#[derive(Default, Clone, Debug)]
pub struct CallOptions {
    /// Thinking level set by /think command.
    /// 0 = off. For adaptive models (Opus 4.6+): 1–2 = low, 3 = medium, 4+ = high effort.
    /// For manual models (Sonnet 3.7, Haiku 4.5, etc.): 1→1024, 2→4096, 3→10000, 4→20000, 5+→32000 budget_tokens.
    pub thinking_level: u8,
    /// Optional CLAUDE.md content for the system agent.
    ///
    /// When `Some(text)` AND the provider supports prompt caching AND
    /// `prompt_cache` is enabled, this text is emitted as a SEPARATE
    /// content block in the request's `system` field with its own
    /// `cache_control: ephemeral` breakpoint — the third stable cache
    /// segment (after system prompt and tool definitions).
    ///
    /// `None` for non-base agents and for agents without prompt caching.
    /// Non-Anthropic providers ignore this field (CACHE-04). See CACHE-02.
    pub claude_md_content: Option<String>,
}

/// Pluggable LLM provider trait.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        opts: CallOptions,
    ) -> Result<LlmResponse>;

    /// Streaming chat: sends content chunks via mpsc channel.
    /// Returns the complete `LlmResponse` when done.
    ///
    /// The channel is bounded (capacity = 1024) so slow downstream consumers
    /// provide backpressure instead of allowing unbounded memory growth.
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::Sender<String>,
        opts: CallOptions,
    ) -> Result<LlmResponse> {
        // Default: fall back to non-streaming and send entire content at once
        let response = self.chat(messages, tools, opts).await?;
        if response.tool_calls.is_empty() {
            let filtered = super::thinking::strip_thinking(&response.content);
            if !filtered.is_empty() {
                chunk_tx.send(filtered).await.ok();
            }
        }
        Ok(response)
    }

    fn name(&self) -> &str;

    /// Override the model for subsequent calls. None clears the override.
    fn set_model_override(&self, _model: Option<String>) {}

    /// Return the effective model name (override or default).
    fn current_model(&self) -> String {
        self.name().to_string()
    }

    /// Maximum wall-clock duration for all retry attempts combined (seconds). 0 = infinite.
    fn run_max_duration_secs(&self) -> u64 {
        0
    }

    /// True when the provider supports Anthropic-style assistant prefill
    /// (injecting a partial assistant message so the model continues from it).
    fn supports_prefill(&self) -> bool {
        false
    }

    /// Probe the provider's API to discover the real context-window size for `model`.
    /// Returns `None` when the provider doesn't expose this information — callers fall
    /// back to the name-based heuristic in `default_context_for_model`.
    /// Results are cached by the caller; implementations should not cache internally.
    async fn context_limit_hint(&self, _model: &str) -> Option<u32> {
        None
    }
}

// ── Skill trigger detection ───────────────────────────────────────────────────

/// Returns `Some("skill_use")` when the system message contains the
/// "## Relevant Skill Detected" marker AND the tools list includes `skill_use`.
/// Providers use this to set `tool_choice` so the model is forced to load the
/// skill before answering. Injected server-side by `context_builder.rs` block 4d.
pub(crate) fn forced_skill_tool(messages: &[Message], tools: &[ToolDefinition]) -> Option<&'static str> {
    let has_trigger = messages
        .iter()
        .any(|m| m.role == MessageRole::System && m.content.contains("## Relevant Skill Detected"));
    if has_trigger && tools.iter().any(|t| t.name == "skill_use") {
        Some("skill_use")
    } else {
        None
    }
}

// ── ModelOverride ─────────────────────────────────────────────────────────────

/// Shared model-override logic: stores a default model name and an optional
/// runtime override (set via `/model` command). Eliminates identical code
/// across `OpenAI`, Anthropic, and Google providers.
pub(crate) struct ModelOverride {
    default: String,
    current: std::sync::RwLock<Option<String>>,
}

impl std::fmt::Display for ModelOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.effective())
    }
}

impl ModelOverride {
    pub fn new(default: String) -> Self {
        Self {
            default,
            current: std::sync::RwLock::new(None),
        }
    }

    /// Return the override if set, otherwise the default.
    pub fn effective(&self) -> String {
        self.current
            .read()
            .unwrap_or_else(|e| {
                tracing::warn!("ModelOverride RwLock poisoned on read, recovering");
                e.into_inner()
            })
            .clone()
            .unwrap_or_else(|| self.default.clone())
    }

    /// Set or clear the runtime override.
    pub fn set(&self, model: Option<String>) {
        *self.current.write().unwrap_or_else(|e| {
            tracing::warn!("ModelOverride RwLock poisoned on write, recovering");
            e.into_inner()
        }) = model;
    }
}

/// Known OpenAI-compatible providers: (name, `default_base_url`, `api_key_env`).
/// `base_url` is the base URL without path — chat path is resolved via `resolve_chat_url()`.
pub(crate) const OPENAI_COMPAT_PROVIDERS: &[(&str, &str, &str)] = &[
    ("minimax",    "https://api.minimax.io",         "MINIMAX_API_KEY"),
    ("deepseek",   "https://api.deepseek.com",       "DEEPSEEK_API_KEY"),
    ("groq",       "https://api.groq.com/openai",    "GROQ_API_KEY"),
    ("together",   "https://api.together.xyz",       "TOGETHER_API_KEY"),
    ("openrouter", "https://openrouter.ai/api",      "OPENROUTER_API_KEY"),
    ("mistral",    "https://api.mistral.ai",         "MISTRAL_API_KEY"),
    ("xai",        "https://api.x.ai",               "XAI_API_KEY"),
    ("perplexity", "https://api.perplexity.ai",      "PERPLEXITY_API_KEY"),
];

/// Vault secret name for all provider credentials (scoped by provider UUID).
pub(crate) const PROVIDER_CREDENTIALS: &str = "PROVIDER_CREDENTIALS";

/// Legacy vault secret name — kept only for migration lookups.
pub(crate) const LLM_CREDENTIALS: &str = "LLM_CREDENTIALS";

/// Resolve API key from vault-scoped credential, falling back to legacy secret name.
pub(crate) async fn resolve_credential(
    secrets: &SecretsManager,
    credential_scope: Option<&str>,
    fallback_name: &str,
) -> Option<String> {
    if let Some(scope) = credential_scope
        && let Some(val) = secrets.get_scoped(PROVIDER_CREDENTIALS, scope).await {
            return Some(val);
        }
    if !fallback_name.is_empty() {
        return secrets.get(fallback_name).await;
    }
    None
}

/// Build request + streaming HTTP clients from the timeout config.
/// Request client has both `connect_timeout` and `request_timeout`.
/// Streaming client has only `connect_timeout` — request body is governed by
/// `stream_inactivity_secs` / `stream_max_duration_secs` (spec §4.2.1).
///
/// Consumed by `*::new_from_row` in each provider impl — threaded from
/// `build_provider(row, timeouts, ...)`.
pub(crate) fn build_provider_clients(timeouts: &TimeoutsConfig) -> (reqwest::Client, reqwest::Client) {
    let connect = std::time::Duration::from_secs(timeouts.connect_secs);
    let request_timeout = if timeouts.request_secs == 0 {
        // 0 = no limit (legacy convention preserved)
        std::time::Duration::from_secs(u64::MAX / 1000)
    } else {
        std::time::Duration::from_secs(timeouts.request_secs)
    };
    let request_client = reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(request_timeout)
        .build()
        .expect("request client builds");
    let streaming_client = reqwest::Client::builder()
        .connect_timeout(connect)
        .build()
        .expect("streaming client builds");
    (request_client, streaming_client)
}

// ── OpenAI wire format helpers ──────────────────────────────────────────────

/// Transform internal Message structs to `OpenAI` API wire format.
/// Key differences from serde default:
///
/// - `tool_calls`: wrapped in `{type: "function", function: {name, arguments_as_string}}`
/// - Remove tool messages whose `tool_call_id` has no preceding assistant message with a
///   matching tool call. This prevents MiniMax/OpenAI "tool result does not follow tool call"
///   errors caused by history truncation cutting off the assistant message while keeping the
///   tool result.
pub(super) fn strip_orphaned_tool_messages(messages: &[Message]) -> Vec<Message> {
    use opex_types::ids::ToolCallId;

    // Pass 1: collect all tool_call_ids that have a saved tool result.
    let mut result_ids = std::collections::HashSet::<ToolCallId>::new();
    for msg in messages {
        if msg.role == MessageRole::Tool
            && let Some(ref id) = msg.tool_call_id {
                result_ids.insert(id.clone());
            }
    }

    // Pass 2: rebuild messages, skipping incomplete assistant+tool_calls groups
    // (where some tool results are missing — e.g. process crashed after saving assistant msg).
    let mut valid_call_ids = std::collections::HashSet::<ToolCallId>::new();
    let mut result = Vec::with_capacity(messages.len());

    for msg in messages {
        match msg.role {
            MessageRole::Assistant => {
                if let Some(ref tcs) = msg.tool_calls
                    && !tcs.is_empty() {
                        let complete = tcs.iter().all(|tc| result_ids.contains(&tc.id));
                        if !complete {
                            tracing::warn!(
                                "dropping assistant+tool_calls message: \
                                 some tool results missing from history (incomplete save)"
                            );
                            continue;
                        }
                        for tc in tcs {
                            valid_call_ids.insert(tc.id.clone());
                        }
                    }
                result.push(msg.clone());
            }
            MessageRole::Tool => {
                let id_str = msg.tool_call_id.as_ref().map(|id| id.as_str()).unwrap_or("");
                let in_valid = msg.tool_call_id
                    .as_ref()
                    .is_some_and(|id| valid_call_ids.contains(id));
                if in_valid {
                    result.push(msg.clone());
                } else {
                    tracing::warn!(
                        tool_call_id = id_str,
                        "dropping orphaned tool message (no preceding tool_call in context)"
                    );
                }
            }
            _ => result.push(msg.clone()),
        }
    }

    result
}

/// - tool messages: include `tool_call_id` at top level
/// - assistant content must be null (not empty string) when only `tool_calls` present
/// - `include_reasoning`: pass `true` only for DeepSeek — other providers reject unknown fields
pub(super) fn messages_to_openai_format(messages: &[Message], include_reasoning: bool) -> Vec<serde_json::Value> {
    let messages = strip_orphaned_tool_messages(messages);
    messages
        .iter()
        .map(|msg| {
            let mut m = serde_json::Map::new();
            m.insert(
                "role".to_string(),
                serde_json::to_value(&msg.role).unwrap_or_default(),
            );

            // Assistant with tool_calls: content can be null
            if msg.role == MessageRole::Assistant
                && let Some(ref tool_calls) = msg.tool_calls
                    && !tool_calls.is_empty() {
                        if msg.content.is_empty() {
                            m.insert("content".to_string(), serde_json::Value::Null);
                        } else {
                            m.insert(
                                "content".to_string(),
                                serde_json::Value::String(msg.content.clone()),
                            );
                        }

                        let tc_json: Vec<serde_json::Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id.as_str(),
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": serde_json::to_string(&tc.arguments)
                                            .unwrap_or_else(|_| "{}".to_string())
                                    }
                                })
                            })
                            .collect();
                        m.insert(
                            "tool_calls".to_string(),
                            serde_json::Value::Array(tc_json),
                        );

                        // DeepSeek: pass reasoning_content back on tool-calling turns.
                        // Gated by include_reasoning — other providers reject unknown fields.
                        if include_reasoning {
                            let reasoning: String = msg.thinking_blocks.iter()
                                .map(|tb| tb.thinking.as_str())
                                .collect::<Vec<_>>()
                                .join("\n");
                            m.insert(
                                "reasoning_content".to_string(),
                                serde_json::Value::String(reasoning),
                            );
                        }

                        return serde_json::Value::Object(m);
                    }

            m.insert(
                "content".to_string(),
                serde_json::Value::String(msg.content.clone()),
            );

            if let Some(ref tool_call_id) = msg.tool_call_id {
                m.insert(
                    "tool_call_id".to_string(),
                    serde_json::Value::String(tool_call_id.as_str().to_string()),
                );
            }

            // DeepSeek: pass reasoning_content back for assistant messages that have thinking blocks.
            // Gated by include_reasoning — other providers reject unknown fields.
            if include_reasoning && msg.role == MessageRole::Assistant && !msg.thinking_blocks.is_empty() {
                let reasoning: String = msg.thinking_blocks.iter()
                    .map(|tb| tb.thinking.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                m.insert(
                    "reasoning_content".to_string(),
                    serde_json::Value::String(reasoning),
                );
            }

            serde_json::Value::Object(m)
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::{Message, MessageRole, ToolCall};

    // ── helpers ──────────────────────────────────────────────────────────────

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

    fn assistant_msg(content: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn assistant_with_calls(content: &str, calls: Vec<(&str, &str)>) -> Message {
        let tool_calls = calls
            .into_iter()
            .map(|(id, name)| ToolCall {
                id: id.into(),
                name: name.to_string(),
                arguments: serde_json::json!({}),
                thought_signature: None,
            })
            .collect();
        Message {
            role: MessageRole::Assistant,
            content: content.to_string(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn tool_msg(call_id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn system_msg(content: &str) -> Message {
        Message {
            role: MessageRole::System,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    // ── ModelOverride tests ───────────────────────────────────────────────────

    #[test]
    fn model_override_new_returns_default() {
        let mo = ModelOverride::new("gpt-4".to_string());
        assert_eq!(mo.effective(), "gpt-4");
    }

    #[test]
    fn model_override_set_some_overrides_default() {
        let mo = ModelOverride::new("gpt-4".to_string());
        mo.set(Some("claude-3".to_string()));
        assert_eq!(mo.effective(), "claude-3");
    }

    #[test]
    fn model_override_set_none_reverts_to_default() {
        let mo = ModelOverride::new("gpt-4".to_string());
        mo.set(Some("claude-3".to_string()));
        mo.set(None);
        assert_eq!(mo.effective(), "gpt-4");
    }

    #[test]
    fn model_override_display_returns_effective() {
        let mo = ModelOverride::new("gpt-4".to_string());
        assert_eq!(format!("{mo}"), "gpt-4");
        mo.set(Some("claude-3".to_string()));
        assert_eq!(format!("{mo}"), "claude-3");
    }

    #[test]
    fn model_override_multiple_sets() {
        let mo = ModelOverride::new("base".to_string());
        mo.set(Some("first".to_string()));
        mo.set(Some("second".to_string()));
        assert_eq!(mo.effective(), "second");
    }

    // ── parse_anthropic_response tests ───────────────────────────────────────

    fn text_block(text: &str) -> AnthropicContentBlock {
        AnthropicContentBlock::Text { text: text.to_string() }
    }

    fn tool_block(id: &str, name: &str, input: serde_json::Value) -> AnthropicContentBlock {
        AnthropicContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }
    }

    #[test]
    fn parse_anthropic_text_only_no_usage() {
        let resp = AnthropicResponse {
            content: vec![text_block("hello")],
            usage: None,
            stop_reason: None,
        };
        let result = parse_anthropic_response(resp, "claude-3");
        assert_eq!(result.content, "hello");
        assert!(result.tool_calls.is_empty());
        assert!(result.usage.is_none());
        assert_eq!(result.model.as_deref(), Some("claude-3"));
        assert_eq!(result.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn parse_anthropic_tool_use_only() {
        let resp = AnthropicResponse {
            content: vec![tool_block("call-1", "search", serde_json::json!({"q": "rust"}))],
            usage: None,
            stop_reason: None,
        };
        let result = parse_anthropic_response(resp, "claude-3");
        assert_eq!(result.content, "");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id.as_str(), "call-1");
        assert_eq!(result.tool_calls[0].name, "search");
        assert_eq!(result.tool_calls[0].arguments, serde_json::json!({"q": "rust"}));
    }

    #[test]
    fn parse_anthropic_mixed_text_and_tool() {
        let resp = AnthropicResponse {
            content: vec![
                text_block("a"),
                tool_block("c1", "do_thing", serde_json::json!({})),
                text_block("b"),
            ],
            usage: None,
            stop_reason: None,
        };
        let result = parse_anthropic_response(resp, "model");
        // texts are joined with \n
        assert_eq!(result.content, "a\nb");
        assert_eq!(result.tool_calls.len(), 1);
    }

    #[test]
    fn parse_anthropic_with_usage() {
        let resp = AnthropicResponse {
            content: vec![text_block("hi")],
            usage: Some(AnthropicUsage { input_tokens: 10, output_tokens: 20, cache_creation_input_tokens: None, cache_read_input_tokens: None }),
            stop_reason: None,
        };
        let result = parse_anthropic_response(resp, "model");
        let usage = result.usage.expect("usage should be Some");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 20);
    }

    #[test]
    fn parse_anthropic_other_block_ignored() {
        let resp = AnthropicResponse {
            content: vec![AnthropicContentBlock::Other],
            usage: None,
            stop_reason: None,
        };
        let result = parse_anthropic_response(resp, "model");
        assert_eq!(result.content, "");
        assert!(result.tool_calls.is_empty());
    }

    #[test]
    fn parse_anthropic_empty_content() {
        let resp = AnthropicResponse {
            content: vec![],
            usage: None,
            stop_reason: None,
        };
        let result = parse_anthropic_response(resp, "model");
        assert_eq!(result.content, "");
        assert!(result.tool_calls.is_empty());
        assert!(result.usage.is_none());
    }

    #[test]
    fn parse_anthropic_multiple_tool_calls() {
        let resp = AnthropicResponse {
            content: vec![
                tool_block("c1", "tool_a", serde_json::json!({"x": 1})),
                tool_block("c2", "tool_b", serde_json::json!({"y": 2})),
            ],
            usage: Some(AnthropicUsage { input_tokens: 5, output_tokens: 15, cache_creation_input_tokens: None, cache_read_input_tokens: None }),
            stop_reason: None,
        };
        let result = parse_anthropic_response(resp, "m");
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].id.as_str(), "c1");
        assert_eq!(result.tool_calls[1].id.as_str(), "c2");
    }

    // ── strip_orphaned_tool_messages tests ───────────────────────────────────

    #[test]
    fn strip_no_tool_messages_unchanged() {
        let msgs = vec![user_msg("hi"), assistant_msg("hello")];
        let result = strip_orphaned_tool_messages(&msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hi");
        assert_eq!(result[1].content, "hello");
    }

    #[test]
    fn strip_complete_pair_kept() {
        let msgs = vec![
            user_msg("go"),
            assistant_with_calls("", vec![("tc1", "tool_x")]),
            tool_msg("tc1", "result"),
        ];
        let result = strip_orphaned_tool_messages(&msgs);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn strip_orphaned_tool_message_dropped() {
        // Tool message with no matching assistant tool_call
        let msgs = vec![user_msg("hi"), tool_msg("tc1", "orphan result")];
        let result = strip_orphaned_tool_messages(&msgs);
        // orphaned tool dropped, user kept
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, MessageRole::User);
    }

    #[test]
    fn strip_incomplete_assistant_dropped_with_tool() {
        // assistant requested tc1 and tc2, but only tc1 result exists
        let msgs = vec![
            user_msg("go"),
            assistant_with_calls("", vec![("tc1", "a"), ("tc2", "b")]),
            tool_msg("tc1", "res1"),
        ];
        let result = strip_orphaned_tool_messages(&msgs);
        // assistant dropped (tc2 missing), tc1 tool also dropped (no valid call)
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, MessageRole::User);
    }

    #[test]
    fn strip_empty_input_returns_empty() {
        let result = strip_orphaned_tool_messages(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn strip_system_and_user_always_kept() {
        let msgs = vec![system_msg("sys"), user_msg("usr")];
        let result = strip_orphaned_tool_messages(&msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, MessageRole::System);
        assert_eq!(result[1].role, MessageRole::User);
    }

    #[test]
    fn strip_assistant_no_tool_calls_kept() {
        // Assistant message without tool_calls is always kept
        let msgs = vec![user_msg("hi"), assistant_msg("plain reply")];
        let result = strip_orphaned_tool_messages(&msgs);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn strip_two_complete_pairs_both_kept() {
        let msgs = vec![
            user_msg("q1"),
            assistant_with_calls("", vec![("tc1", "tool_a")]),
            tool_msg("tc1", "res1"),
            user_msg("q2"),
            assistant_with_calls("", vec![("tc2", "tool_b")]),
            tool_msg("tc2", "res2"),
        ];
        let result = strip_orphaned_tool_messages(&msgs);
        assert_eq!(result.len(), 6);
    }

    // ── contains_any tests ────────────────────────────────────────────────────

    #[test]
    fn contains_any_match_found_returns_true() {
        assert!(super::routing::contains_any("write a script", &["script", "code"]));
    }

    #[test]
    fn contains_any_no_match_returns_false() {
        assert!(!super::routing::contains_any("hello world", &["script", "code", "execute"]));
    }

    #[test]
    fn contains_any_empty_keywords_returns_false() {
        assert!(!super::routing::contains_any("anything goes here", &[]));
    }

    // ── messages_to_gemini_format tests ───────────────────────────────────────

    #[test]
    fn gemini_system_extracted_user_and_assistant_mapped() {
        let msgs = vec![
            system_msg("You are helpful."),
            user_msg("Hello"),
            assistant_msg("Hi there!"),
        ];
        let (system, contents) = messages_to_gemini_format(&msgs);
        assert_eq!(system.as_deref(), Some("You are helpful."));
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "Hi there!");
    }

    #[test]
    fn gemini_tool_message_becomes_function_response() {
        let msgs = vec![
            assistant_with_calls("", vec![("tc1", "get_weather")]),
            tool_msg("tc1", "Sunny, 25°C"),
        ];
        let (_system, contents) = messages_to_gemini_format(&msgs);
        // second item is the tool result
        let tool_content = &contents[1];
        assert_eq!(tool_content["role"], "user");
        let fr = &tool_content["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "tc1");
        assert_eq!(fr["response"]["result"], "Sunny, 25°C");
    }

    #[test]
    fn gemini_assistant_with_tool_calls_becomes_function_call_parts() {
        let msgs = vec![assistant_with_calls("Thinking...", vec![("tc1", "search")])];
        let (_system, contents) = messages_to_gemini_format(&msgs);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");
        // non-empty content produces a text part first, then functionCall
        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "Thinking...");
        assert!(parts[1].get("functionCall").is_some());
        assert_eq!(parts[1]["functionCall"]["name"], "search");
    }

    #[test]
    fn gemini_empty_messages_returns_none_and_empty_vec() {
        let (system, contents) = messages_to_gemini_format(&[]);
        assert!(system.is_none());
        assert!(contents.is_empty());
    }

    // ── messages_to_openai_format tests ──────────────────────────────────────

    #[test]
    fn openai_basic_user_and_assistant_messages() {
        let msgs = vec![user_msg("Hello"), assistant_msg("Hi!")];
        let result = messages_to_openai_format(&msgs, false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["role"], "user");
        assert_eq!(result[0]["content"], "Hello");
        assert_eq!(result[1]["role"], "assistant");
        assert_eq!(result[1]["content"], "Hi!");
    }

    #[test]
    fn openai_assistant_with_tool_calls_has_tool_calls_array_and_null_content() {
        let msgs = vec![
            user_msg("go"),
            assistant_with_calls("", vec![("tc1", "search")]),
            tool_msg("tc1", "result"),
        ];
        let result = messages_to_openai_format(&msgs, false);
        let asst = &result[1];
        assert_eq!(asst["content"], serde_json::Value::Null);
        let tc_arr = asst["tool_calls"].as_array().expect("tool_calls array");
        assert_eq!(tc_arr.len(), 1);
        assert_eq!(tc_arr[0]["id"], "tc1");
        assert_eq!(tc_arr[0]["type"], "function");
        assert_eq!(tc_arr[0]["function"]["name"], "search");
    }

    #[test]
    fn openai_tool_message_includes_tool_call_id() {
        let msgs = vec![
            user_msg("go"),
            assistant_with_calls("", vec![("call-42", "my_tool")]),
            tool_msg("call-42", "tool output"),
        ];
        let result = messages_to_openai_format(&msgs, false);
        let tool = &result[2];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["content"], "tool output");
        assert_eq!(tool["tool_call_id"], "call-42");
    }

    #[test]
    fn openai_system_message_preserved() {
        let msgs = vec![system_msg("You are an AI."), user_msg("Hi")];
        let result = messages_to_openai_format(&msgs, false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["role"], "system");
        assert_eq!(result[0]["content"], "You are an AI.");
    }

    #[test]
    fn openai_assistant_with_content_and_tool_calls_preserves_content() {
        let msgs = vec![
            user_msg("go"),
            assistant_with_calls("Let me search for that.", vec![("tc1", "search")]),
            tool_msg("tc1", "found it"),
        ];
        let result = messages_to_openai_format(&msgs, false);
        let asst = &result[1];
        // non-empty content should be preserved (not null)
        assert_eq!(asst["content"], "Let me search for that.");
        assert!(asst.get("tool_calls").is_some());
    }
}

#[cfg(test)]
mod call_options_tests {
    use super::*;

    #[test]
    fn call_options_default_thinking_level_is_zero() {
        let opts = CallOptions::default();
        assert_eq!(opts.thinking_level, 0);
    }
}
