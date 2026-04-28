use anyhow::Result;
use async_trait::async_trait;
use hydeclaw_types::{LlmResponse, Message, MessageRole, ToolDefinition};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::agent::cli_backend;
use crate::secrets::SecretsManager;

// Extracted provider implementations (submodules for full super:: access)
#[path = "providers_openai.rs"]
mod openai_impl;
use openai_impl::OpenAiCompatibleProvider;
#[path = "providers_anthropic.rs"]
mod anthropic_impl;
use anthropic_impl::AnthropicProvider;
#[cfg(test)]
use anthropic_impl::{AnthropicContentBlock, AnthropicResponse, AnthropicUsage, parse_anthropic_response};
#[path = "providers_google.rs"]
mod google_impl;
use google_impl::GoogleProvider;
#[cfg(test)]
use google_impl::messages_to_gemini_format;
#[path = "providers_claude_cli.rs"]
mod claude_cli_impl;
use claude_cli_impl::ClaudeCliProvider;

pub mod timeouts;
#[allow(unused_imports)] // first consumer arrives in Task 2 (ProviderOptions)
pub use timeouts::TimeoutsConfig;

pub mod error;
#[allow(unused_imports)] // first consumer arrives in Task 12 (build_provider)
pub use error::{LlmCallError, CancelReason, classify_reqwest_err};

#[cfg(test)]
mod routing_tests;

#[cfg(test)]
mod build_provider_tests;

pub mod cancellable_stream;
#[allow(unused_imports)] // first consumer arrives in Task 9 (stream_with_cancellation)
pub use cancellable_stream::{CancelSlot, set_and_cancel};

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
    ) -> Result<LlmResponse> {
        Err(self.err())
    }

    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _chunk_tx: mpsc::UnboundedSender<String>,
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

/// Pluggable LLM provider trait.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse>;

    /// Streaming chat: sends content chunks via mpsc channel.
    /// Returns the complete `LlmResponse` when done.
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
    ) -> Result<LlmResponse> {
        // Default: fall back to non-streaming and send entire content at once
        let response = self.chat(messages, tools).await?;
        if response.tool_calls.is_empty() {
            let filtered = super::thinking::strip_thinking(&response.content);
            if !filtered.is_empty() {
                chunk_tx.send(filtered).ok();
            }
        }
        Ok(response)
    }

    #[allow(dead_code)]
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

// NOTE: `create_provider` (free-form provider_name path) was removed in Task 12
// in favor of the unified `build_provider(row, timeouts, cancel)` entry point.
// Legacy callers now go through DB lookup → ProviderRow → build_provider.

/// Create a CLI provider from a preset with DB option overrides.
/// Returns `None` if the preset ID is not recognized.
#[allow(clippy::too_many_arguments, dead_code)]
pub fn create_cli_provider_with_options(
    preset_id: &str,
    model: &str,
    db_options: &serde_json::Value,
    secrets: Arc<SecretsManager>,
    sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    agent_name: &str,
    workspace_dir: &str,
    base: bool,
    api_key: Option<String>,
) -> Option<Arc<dyn LlmProvider>> {
    let config = cli_backend::resolve_cli_config(preset_id, db_options)?;
    Some(Arc::new(ClaudeCliProvider::new(
        preset_id, config, model.to_string(), sandbox, agent_name.to_string(), workspace_dir.to_string(), base, secrets, api_key,
    )))
}

// NOTE: `create_provider_from_route` was removed in Task 12. Routes now carry
// only `connection` + overrides — the route factory goes through DB lookup +
// `build_provider`.

/// Create a routing provider from ordered route configs. Each route references a
/// named DB provider via `connection`; this function resolves each to a
/// `ProviderRow` and builds a provider via `build_provider`.
///
/// `agent_temperature` / `agent_max_tokens` provide agent-level defaults; a
/// route's `temperature` field (if present) takes precedence. `model` override
/// comes from the route's `model` field. These are bundled into
/// `ProviderOverrides` and passed to `build_provider`.
///
/// `max_failover_attempts` caps the number of fallback attempts per request
/// (re-added post-c55b039 / 8d33376 — see issue #9).
///
/// Routes with a missing or invalid `connection` are skipped with a log entry.
#[allow(clippy::too_many_arguments)]
pub async fn create_routing_provider(
    db: &sqlx::PgPool,
    routes: &[crate::config::ProviderRouteConfig],
    agent_temperature: f64,
    agent_max_tokens: Option<u32>,
    max_failover_attempts: u32,
    secrets: Arc<SecretsManager>,
) -> Arc<dyn LlmProvider> {
    let mut entries: Vec<RouteEntry> = Vec::with_capacity(routes.len());
    for r in routes {
        let Some(conn_name) = r.connection.as_deref().filter(|s| !s.is_empty()) else {
            tracing::warn!(condition = %r.condition, "routing rule has no `connection` — skipping");
            continue;
        };
        let row = match crate::db::providers::get_provider_by_name(db, conn_name).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::warn!(condition = %r.condition, connection = %conn_name,
                    "routing rule references missing provider — skipping");
                continue;
            }
            Err(e) => {
                tracing::error!(condition = %r.condition, connection = %conn_name, error = %e,
                    "DB error resolving route connection — skipping");
                continue;
            }
        };
        let opts: timeouts::ProviderOptions =
            serde_json::from_value(row.options.clone()).unwrap_or_default();
        let timeouts_cfg = opts.timeouts;
        let cancel = tokio_util::sync::CancellationToken::new();
        // Route temperature beats agent default; missing route temperature
        // falls through to `agent_temperature` (which is itself agent → global default).
        let effective_temperature = r.temperature.unwrap_or(agent_temperature);
        let model_override = r
            .model
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let overrides = ProviderOverrides {
            model: model_override,
            temperature: Some(effective_temperature),
            max_tokens: agent_max_tokens,
            prompt_cache: None,
        };
        let p = match build_provider(&row, secrets.clone(), &timeouts_cfg, cancel, overrides) {
            Ok(p) => {
                let arc: Arc<dyn LlmProvider> = Arc::from(p);
                arc
            }
            Err(e) => {
                tracing::error!(condition = %r.condition, connection = %conn_name, error = %e,
                    "failed to build provider from route — skipping");
                continue;
            }
        };
        let key = format!("{}:{}", r.condition, p.name());
        entries.push(RouteEntry {
            condition: r.condition.clone(),
            key,
            provider: p,
            cooldown_duration: std::time::Duration::from_secs(r.cooldown_secs.max(1)),
        });
    }

    // Issue #2: if every configured route was skipped (missing `connection`,
    // missing DB row, or `build_provider` failure), install a sentinel
    // `UnconfiguredProvider` entry so `select_route` always has something
    // to return. Without this, the first `chat()` would panic in
    // `select_route` via `.expect("RoutingProvider has no routes")`.
    //
    // The sentinel returns `LlmCallError::AuthError` (classified,
    // non-failover-worthy) on every call — matching the degraded-path
    // pattern used by `resolve_provider_for_agent`. Every call now surfaces
    // a consistent typed error instead of panicking.
    if entries.is_empty() {
        tracing::error!(
            attempted_routes = routes.len(),
            "RoutingProvider has no usable routes — installing \
             `unconfigured` sentinel; all LLM calls for this agent will \
             return a classified error until a working route is added"
        );
        let sentinel: Arc<dyn LlmProvider> = Arc::new(UnconfiguredProvider::new(
            "no usable routes",
        ));
        entries.push(RouteEntry {
            condition: "default".to_string(),
            key: "unconfigured:sentinel".to_string(),
            provider: sentinel,
            cooldown_duration: std::time::Duration::from_secs(1),
        });
    }

    Arc::new(RoutingProvider {
        routes: entries,
        cooldowns: std::sync::Mutex::new(std::collections::HashMap::new()),
        max_failover_attempts,
    })
}

/// Build full chat completions URL from `base_url` + provider's `chat_path`.
pub fn resolve_chat_url(provider_type: &str, base_url: &str) -> String {
    let chat_path = PROVIDER_TYPES.iter()
        .find(|pt| pt.id == provider_type)
        .map_or("/v1/chat/completions", |pt| pt.chat_path);
    if chat_path.is_empty() {
        return base_url.to_string();
    }
    format!("{}{}", base_url.trim_end_matches('/'), chat_path)
}

/// Default base URL for a provider type (from `PROVIDER_TYPES`).
#[allow(dead_code)]
pub fn default_base_url_for_type(provider_type: &str) -> &'static str {
    PROVIDER_TYPES.iter()
        .find(|pt| pt.id == provider_type)
        .map_or("", |pt| pt.default_base_url)
}

// ── Named connection provider types ───────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderTypeMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub chat_path: &'static str,
    pub default_base_url: &'static str,
    pub default_secret_name: &'static str,
    pub requires_api_key: bool,
    pub supports_model_listing: bool,
    /// For CLI providers: delegate model listing to this provider type's API
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_provider: Option<&'static str>,
    /// Hardcoded fallback models when runtime fetch fails
    pub default_models: &'static [&'static str],
}

/// Known provider types with extended metadata.
pub(crate) const PROVIDER_TYPES: &[ProviderTypeMeta] = &[
    ProviderTypeMeta {
        id: "minimax",
        name: "MiniMax",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.minimax.io",
        default_secret_name: "MINIMAX_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "openai",
        name: "OpenAI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.openai.com",
        default_secret_name: "OPENAI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "anthropic",
        name: "Anthropic",
        chat_path: "",
        default_base_url: "https://api.anthropic.com",
        default_secret_name: "ANTHROPIC_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "google",
        name: "Google Gemini",
        chat_path: "",
        default_base_url: "https://generativelanguage.googleapis.com",
        default_secret_name: "GOOGLE_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "deepseek",
        name: "DeepSeek",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.deepseek.com",
        default_secret_name: "DEEPSEEK_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "groq",
        name: "Groq",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.groq.com/openai",
        default_secret_name: "GROQ_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "openrouter",
        name: "OpenRouter",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://openrouter.ai/api",
        default_secret_name: "OPENROUTER_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "mistral",
        name: "Mistral",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.mistral.ai",
        default_secret_name: "MISTRAL_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "xai",
        name: "xAI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.x.ai",
        default_secret_name: "XAI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "perplexity",
        name: "Perplexity",
        chat_path: "/chat/completions",
        default_base_url: "https://api.perplexity.ai",
        default_secret_name: "PERPLEXITY_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "together",
        name: "Together AI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.together.xyz",
        default_secret_name: "TOGETHER_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "ollama",
        name: "Ollama",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:11434",
        default_secret_name: "",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "openai_compat",
        name: "OpenAI Compatible",
        chat_path: "/v1/chat/completions",
        default_base_url: "",
        default_secret_name: "API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "claude-cli",
        name: "Claude CLI",
        chat_path: "",
        default_base_url: "",
        default_secret_name: "ANTHROPIC_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: Some("anthropic"),
        default_models: &["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"],
    },
    ProviderTypeMeta {
        id: "gemini-cli",
        name: "Gemini CLI",
        chat_path: "",
        default_base_url: "",
        default_secret_name: "GEMINI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: Some("google"),
        default_models: &["gemini-3.1-pro-preview", "gemini-3-flash-preview", "gemini-2.5-flash", "gemini-2.5-pro"],
    },
    ProviderTypeMeta {
        id: "codex-cli",
        name: "Codex CLI",
        chat_path: "",
        default_base_url: "",
        default_secret_name: "OPENAI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: Some("openai"),
        default_models: &["codex-mini", "gpt-4.1", "o4-mini"],
    },
    // ── Additional OpenAI-compatible providers ──────────────────────────────
    ProviderTypeMeta {
        id: "huggingface",
        name: "Hugging Face",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api-inference.huggingface.co",
        default_secret_name: "HF_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "moonshot",
        name: "Moonshot AI (Kimi)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.moonshot.cn",
        default_secret_name: "MOONSHOT_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "nvidia",
        name: "NVIDIA",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://integrate.api.nvidia.com",
        default_secret_name: "NVIDIA_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "venice",
        name: "Venice AI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.venice.ai",
        default_secret_name: "VENICE_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "cloudflare",
        name: "Cloudflare AI Gateway",
        chat_path: "/v1/chat/completions",
        default_base_url: "",
        default_secret_name: "CF_AI_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "litellm",
        name: "LiteLLM",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:4000",
        default_secret_name: "LITELLM_API_KEY",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "volcengine",
        name: "Volcengine (Doubao)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://ark.cn-beijing.volces.com/api",
        default_secret_name: "VOLCENGINE_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "qwen",
        name: "Qwen (Alibaba)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://dashscope.aliyuncs.com/compatible-mode",
        default_secret_name: "DASHSCOPE_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "glm",
        name: "GLM (Zhipu AI)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://open.bigmodel.cn/api/paas",
        default_secret_name: "GLM_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "sglang",
        name: "SGLang",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:30000",
        default_secret_name: "",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "vllm",
        name: "vLLM",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:8000",
        default_secret_name: "",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "qianfan",
        name: "Qianfan (Baidu)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://qianfan.baidubce.com",
        default_secret_name: "QIANFAN_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "xiaomi",
        name: "Xiaomi MiLM",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.ai.xiaomi.com",
        default_secret_name: "XIAOMI_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
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

/// Per-call overrides threaded from agent/route config into `build_provider`.
///
/// `None` fields mean "fall back to the row/options default" — see
/// `new_from_row` impls for the default chain (hardcoded 0.7 / None / false
/// are the last-resort fallbacks when neither override nor row supplies a value).
///
/// `prompt_cache` is honored only by `AnthropicProvider`; other providers
/// ignore it.
#[derive(Debug, Clone, Default)]
pub struct ProviderOverrides {
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// Anthropic-only prompt-cache override. `None` → take from
    /// `ProviderOptions.prompt_cache`; `Some(x)` → force `x`.
    pub prompt_cache: Option<bool>,
}

/// Single constructor for LLM providers. Unifies the three legacy paths
/// (`create_provider`, `create_provider_from_connection`, `create_provider_from_route`).
///
/// The `timeouts` + `cancel` parameters are stored on each provider and consumed
/// by `stream_with_cancellation` in `chat_stream`. Request-side clients are
/// built via `build_provider_clients(timeouts)` honoring `connect_secs` and
/// `request_secs` (spec §4.2.1).
///
/// `overrides` threads agent-level and route-level settings — temperature,
/// max_tokens, prompt_cache, model — that are NOT present on `ProviderRow`
/// (which only carries provider-identity fields).
///
/// The task spec signature `build_provider(row, timeouts, cancel)` is extended
/// with `secrets` because every HTTP provider resolves API keys against the
/// vault at call time.
///
/// Note: CLI providers (`claude-cli`, `gemini-cli`, `codex-cli`) need a
/// `CliContext` (sandbox + agent_name + workspace_dir + base) which is NOT
/// part of `ProviderRow`. Callers that need CLI providers must use
/// `build_cli_provider` instead. If a CLI `provider_type` is passed here,
/// `build_provider` returns an error.
pub fn build_provider(
    row: &crate::db::providers::ProviderRow,
    secrets: Arc<SecretsManager>,
    timeouts: &TimeoutsConfig,
    cancel: tokio_util::sync::CancellationToken,
    overrides: ProviderOverrides,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    // Parse options (typed — unknown keys land in `extra`)
    let opts: timeouts::ProviderOptions =
        serde_json::from_value(row.options.clone()).unwrap_or_default();
    timeouts::warn_unknown_keys(&row.name, &opts);

    // Validate options on every construction (spec §4.3). Catches malformed
    // timeouts persisted before validation was wired up OR snuck past the
    // PUT /api/providers endpoint via a code path that bypasses validation.
    if let Err(msg) = opts.validate() {
        anyhow::bail!(
            "provider `{}` has invalid options: {}",
            row.name,
            msg
        );
    }

    match row.provider_type.as_str() {
        "anthropic" => {
            let provider = AnthropicProvider::new_from_row(
                row, secrets, *timeouts, cancel, opts, overrides,
            )?;
            Ok(Box::new(provider))
        }
        "google" | "gemini" => {
            let provider = GoogleProvider::new_from_row(
                row, secrets, *timeouts, cancel, opts, overrides,
            )?;
            Ok(Box::new(provider))
        }
        "claude-cli" | "gemini-cli" | "codex-cli" => {
            anyhow::bail!(
                "build_provider: CLI provider_type `{}` requires a CliContext; use build_cli_provider instead",
                row.provider_type
            );
        }
        // Everything else (openai, ollama, custom-http, and generic OpenAI-compatible) → OpenAiCompatibleProvider
        _ => {
            let provider = OpenAiCompatibleProvider::new_from_row(
                row, secrets, *timeouts, cancel, opts, overrides,
            )?;
            Ok(Box::new(provider))
        }
    }
}

/// Context required to build a CLI provider (claude-cli / gemini-cli / codex-cli).
/// These providers execute an external binary inside the agent's Docker sandbox
/// (or on host for privileged `base` agents) and need runtime information that's
/// not part of `ProviderRow`.
pub struct CliContext<'a> {
    pub sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    pub agent_name: &'a str,
    pub workspace_dir: &'a str,
    pub base: bool,
    pub secrets: Arc<SecretsManager>,
}

/// Build a CLI-backed LLM provider. Companion to `build_provider` for CLI types.
pub async fn build_cli_provider(
    row: &crate::db::providers::ProviderRow,
    model_override: Option<&str>,
    ctx: CliContext<'_>,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    let provider = ClaudeCliProvider::new_from_row(row, model_override, ctx).await?;
    Ok(Box::new(provider))
}

/// Resolve LLM provider for an agent from a named connection in the DB.
/// The agent MUST have `provider_connection` set.
///
/// Returns a sentinel "unconfigured" provider if no usable connection is found.
/// Post-Task 12: no more free-form `provider`-field fallback path — that was the
/// job of the removed `create_provider` function. Agents without a valid
/// `provider_connection` get an `UnconfiguredProvider` sentinel.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_provider_for_agent(
    db: &sqlx::PgPool,
    agent: &crate::config::AgentSettings,
    temperature: f64,
    max_tokens: Option<u32>,
    secrets: Arc<SecretsManager>,
    sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    agent_name: &str,
    workspace_dir: &str,
    base: bool,
) -> Arc<dyn LlmProvider> {
    if let Some(conn_name) = agent.provider_connection.as_deref().filter(|s| !s.is_empty()) {
        match crate::db::providers::get_provider_by_name(db, conn_name).await {
            Ok(Some(row)) if row.category == "text" || row.category == "llm" => {
                tracing::debug!(agent = %agent_name, connection = %conn_name, "using named LLM provider");
                let model_override = if agent.model.is_empty() { None } else { Some(agent.model.as_str()) };
                return resolve_provider_from_row(
                    &row,
                    model_override,
                    temperature,
                    max_tokens,
                    secrets,
                    sandbox,
                    agent_name,
                    workspace_dir,
                    base,
                ).await;
            }
            Ok(Some(row)) => {
                tracing::warn!(agent = %agent_name, connection = %conn_name, category = %row.category,
                    "named provider is not type=text/llm, calls will fail");
            }
            Ok(None) => {
                tracing::warn!(agent = %agent_name, connection = %conn_name,
                    "named provider connection not found in DB");
            }
            Err(e) => {
                tracing::error!(agent = %agent_name, error = %e,
                    "DB error resolving provider connection");
            }
        }
    }

    tracing::error!(agent = %agent_name, "no usable LLM provider configured; calls will fail");
    let _ = (temperature, max_tokens, secrets); // sentinel path; values are consumed
                                                // on the happy path via resolve_provider_from_row.
    Arc::new(UnconfiguredProvider::new("no usable LLM provider configured for agent"))
}

/// Internal dispatch: build a provider from a DB row, applying per-agent model
/// override. Uses `build_provider` for HTTP providers, `build_cli_provider` for
/// CLI providers.
///
/// `temperature` / `max_tokens` are agent-level settings (after
/// `[agent.defaults]` fallback) threaded into `ProviderOverrides` so the
/// HTTP provider's request body carries the correct values (issue #4).
#[allow(clippy::too_many_arguments)]
async fn resolve_provider_from_row(
    row: &crate::db::providers::ProviderRow,
    model_override: Option<&str>,
    temperature: f64,
    max_tokens: Option<u32>,
    secrets: Arc<SecretsManager>,
    sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    agent_name: &str,
    workspace_dir: &str,
    base: bool,
) -> Arc<dyn LlmProvider> {
    let opts: timeouts::ProviderOptions =
        serde_json::from_value(row.options.clone()).unwrap_or_default();
    let timeouts_cfg = opts.timeouts;
    let cancel = tokio_util::sync::CancellationToken::new();

    let provider: Box<dyn LlmProvider> = match row.provider_type.as_str() {
        "claude-cli" | "gemini-cli" | "codex-cli" => {
            let ctx = CliContext {
                sandbox,
                agent_name,
                workspace_dir,
                base,
                secrets: secrets.clone(),
            };
            match build_cli_provider(row, model_override, ctx).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(agent = %agent_name, error = %e, "failed to build CLI provider");
                    Box::new(UnconfiguredProvider::new(format!("CLI provider build failed: {e}")))
                }
            }
        }
        _ => {
            let overrides = ProviderOverrides {
                model: model_override.map(str::to_string),
                temperature: Some(temperature),
                max_tokens,
                prompt_cache: None,
            };
            match build_provider(row, secrets.clone(), &timeouts_cfg, cancel, overrides) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(provider = %row.name, error = %e, "failed to build provider");
                    Box::new(UnconfiguredProvider::new(format!("HTTP provider build failed: {e}")))
                }
            }
        }
    };

    let arc: Arc<dyn LlmProvider> = Arc::from(provider);
    if let Some(m) = model_override
        && !m.is_empty()
    {
        arc.set_model_override(Some(m.to_string()));
    }
    arc
}

// ── RoutingProvider ───────────────────────────────────────────────────────────

struct RouteEntry {
    condition: String,
    /// Unique key for cooldown tracking: "{`condition}:{provider_name`}" to prevent
    /// two routes that use the same provider (but different models/configs) from
    /// sharing a cooldown bucket.
    key: String,
    provider: Arc<dyn LlmProvider>,
    cooldown_duration: std::time::Duration,
}

/// Routing provider: selects the appropriate backend based on message characteristics.
pub struct RoutingProvider {
    routes: Vec<RouteEntry>,
    /// Tracks providers on cooldown (provider name → cooldown expiry).
    cooldowns: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    /// Maximum number of *failover* attempts per request. Does NOT count the
    /// primary call itself — a value of 3 means "up to 3 fallbacks after
    /// primary failed". Re-added to prevent unbounded cascading failures
    /// through long fallback chains (see issue #9 / commit c55b039).
    max_failover_attempts: u32,
}

impl RoutingProvider {
    /// Choose the best matching provider for the given messages and tools.
    /// Evaluates conditions in order; returns the first match.
    /// Falls back to the last route if nothing else matches.
    fn select_route(
        &self,
        messages: &[hydeclaw_types::Message],
        tools: &[hydeclaw_types::ToolDefinition],
    ) -> Result<&RouteEntry> {
        let last_user_msg = messages
            .iter()
            .rev()
            .find(|m| m.role == hydeclaw_types::MessageRole::User)
            .map_or("", |m| m.content.as_str());

        let last_user_len = last_user_msg.len();
        let lower = last_user_msg.to_lowercase();

        for entry in &self.routes {
            let matches = match entry.condition.as_str() {
                "short" => last_user_len < 300,
                "long" => last_user_len > 2000,
                "with_tools" => !tools.is_empty(),
                "financial" => contains_any(&lower, FINANCIAL_KEYWORDS),
                "analytical" => contains_any(&lower, ANALYTICAL_KEYWORDS),
                "code" => contains_any(&lower, CODE_KEYWORDS),
                "default" | "always" => true,
                "fallback" => false, // only used via explicit fallback logic below
                _ => false,
            };
            if matches {
                tracing::debug!(condition = %entry.condition, "routing condition matched");
                return Ok(entry);
            }
        }

        // Last resort: return last route (or first if routes is empty —
        // shouldn't happen because `create_routing_provider` installs a
        // sentinel `UnconfiguredProvider` when the route list would
        // otherwise be empty, so this branch is unreachable in prod.
        // Belt + suspenders: return an error instead of panicking.
        self.routes.last()
            .or_else(|| self.routes.first())
            .ok_or_else(|| anyhow::anyhow!(
                "RoutingProvider has no routes — this indicates a bug in \
                 create_routing_provider (sentinel was not installed)"
            ))
    }

    /// Check if a provider is on cooldown.
    fn is_on_cooldown(&self, name: &str) -> bool {
        let map = self.cooldowns.lock().unwrap_or_else(|e| {
            tracing::warn!("cooldowns Mutex poisoned, recovering");
            e.into_inner()
        });
        map.get(name).is_some_and(|exp| std::time::Instant::now() < *exp)
    }

    /// Put a provider on cooldown.
    fn set_cooldown(&self, name: &str, duration: std::time::Duration) {
        let mut map = self.cooldowns.lock().unwrap_or_else(|e| {
            tracing::warn!("cooldowns Mutex poisoned on write, recovering");
            e.into_inner()
        });
        map.insert(name.to_string(), std::time::Instant::now() + duration);
    }

    /// Classify error and apply appropriate cooldown.
    ///
    /// Returns `Some(reason)` if the error is failover-worthy (caller
    /// should try next route — the returned short string is the label
    /// suitable for `llm_failover_total{reason=…}`), or `None` if the
    /// error should bubble up immediately (preserving any `partial_state`
    /// carried by the typed `LlmCallError`).
    ///
    /// Resolution order:
    /// 1. Downcast to `LlmCallError` and honor `is_failover_worthy()`.
    ///    - `AuthError` failover is disabled per the typed predicate, but
    ///      when the error is classified via the legacy string path we still
    ///      apply the 300s cooldown floor documented in spec §4.6.
    /// 2. If the downcast fails, fall back to the legacy string-based
    ///    classification. Untyped errors are treated as failover-worthy to
    ///    preserve historical behavior.
    ///
    /// Side effect: bumps `metrics::MetricsRegistry::record_llm_timeout`
    /// when the error is one of the four `LlmCallError` timeout variants
    /// (connect / request / inactivity / max_duration). The failover
    /// counter itself is bumped by the caller once it knows the target
    /// route (`from → to`).
    fn handle_provider_error(
        &self,
        e: &anyhow::Error,
        provider_name: &str,
        route_cooldown: std::time::Duration,
    ) -> Option<&'static str> {
        if let Some(llm_err) = e.downcast_ref::<LlmCallError>() {
            // Bump the timeout counter for the four timeout variants,
            // regardless of failover-worthiness (max_duration is NOT
            // failover-worthy but is still a timeout we want to count).
            if let Some(metrics) = crate::metrics::global() {
                match llm_err {
                    LlmCallError::ConnectTimeout { provider, .. } => {
                        metrics.record_llm_timeout(provider, "connect");
                    }
                    LlmCallError::RequestTimeout { provider, .. } => {
                        metrics.record_llm_timeout(provider, "request");
                    }
                    LlmCallError::InactivityTimeout { provider, .. } => {
                        metrics.record_llm_timeout(provider, "inactivity");
                    }
                    LlmCallError::MaxDurationExceeded { provider, .. } => {
                        metrics.record_llm_timeout(provider, "max_duration");
                    }
                    _ => {}
                }
            }

            if !llm_err.is_failover_worthy() {
                // Non-failover-worthy errors bubble up to the caller, but some
                // typed variants (notably `AuthError`) still deserve a cooldown
                // to prevent re-hammering the primary with the same credentials.
                // Issue #8: the 300s floor documented in spec §4.6 was
                // previously only reachable on the legacy string-classified
                // path; the typed path short-circuited before `set_cooldown`.
                if matches!(llm_err, LlmCallError::AuthError { .. }) {
                    let cd = std::time::Duration::from_secs(route_cooldown.as_secs().max(300));
                    tracing::warn!(
                        provider = %provider_name,
                        error = %e,
                        cooldown_secs = cd.as_secs(),
                        "route failed with AuthError — applying 300s cooldown floor (not failing over)"
                    );
                    self.set_cooldown(provider_name, cd);
                } else {
                    tracing::warn!(
                        provider = %provider_name,
                        error = %e,
                        "route failed with non-failover-worthy error — bubbling up"
                    );
                }
                return None;
            }
            let cd = match llm_err {
                LlmCallError::AuthError { .. } => {
                    std::time::Duration::from_secs(route_cooldown.as_secs().max(300))
                }
                _ => route_cooldown.max(std::time::Duration::from_secs(1)),
            };
            let reason: &'static str = match llm_err {
                LlmCallError::ConnectTimeout { .. } => "connect_timeout",
                LlmCallError::RequestTimeout { .. } => "request_timeout",
                LlmCallError::InactivityTimeout { .. } => "inactivity",
                LlmCallError::Server5xx { .. } => "5xx",
                LlmCallError::Network(_) => "network",
                LlmCallError::SchemaError { .. } => "schema_pre_stream",
                // The remaining typed variants are NOT failover-worthy
                // (AuthError, MaxDurationExceeded, UserCancelled,
                // ShutdownDrain) and returned `None` above — unreachable
                // here, but we provide a stable token for defense.
                _ => "typed_other",
            };
            tracing::warn!(
                provider = %provider_name,
                error = %e,
                cooldown_secs = cd.as_secs(),
                reason = reason,
                "route failed (typed), attempting next"
            );
            self.set_cooldown(provider_name, cd);
            return Some(reason);
        }

        // Untyped error: legacy string-based classification.
        let class = super::error_classify::classify(e);
        let cd = super::error_classify::cooldown_duration(&class).min(route_cooldown);
        tracing::warn!(
            provider = %provider_name,
            error = %e,
            error_class = ?class,
            cooldown_secs = cd.as_secs(),
            "route failed (untyped), attempting next"
        );
        if !cd.is_zero() {
            self.set_cooldown(provider_name, cd);
        }
        Some("untyped")
    }

    /// Record a failover transition. Called at the point where the router
    /// has decided the current route failed with a failover-worthy error
    /// and is about to attempt `to_key`. Internally looks up the
    /// process-wide `MetricsRegistry` via `metrics::global()` and is a
    /// no-op if none has been installed (e.g. in unit tests).
    fn record_failover(from_key: &str, to_key: &str, reason: &str) {
        if let Some(metrics) = crate::metrics::global() {
            metrics.record_llm_failover(from_key, to_key, reason);
        }
    }

    /// Get all route entries that could serve as fallbacks (not on cooldown, not excluded).
    fn available_fallbacks(&self, exclude_key: &str) -> Vec<&RouteEntry> {
        self.routes
            .iter()
            .filter(|e| e.key != exclude_key && !self.is_on_cooldown(&e.key))
            .collect()
    }

    /// Test-only constructor for `RoutingProvider` — builds a routing chain from
    /// a list of `(key, provider, cooldown_secs)` tuples without going through
    /// `build_provider` / DB resolution. Used by unit tests for the failover
    /// predicate wiring. Defaults `max_failover_attempts` to a large value
    /// (`u32::MAX`) so existing tests exercise the full route list; use
    /// `new_for_test_with_cap` to verify the cap behavior explicitly.
    ///
    /// Every entry is installed with condition `"default"` so `select_route`
    /// matches on the first one (same behavior the production `always`
    /// condition would give for a single-route chain).
    #[cfg(test)]
    pub(crate) fn new_for_test(routes: Vec<(String, Arc<dyn LlmProvider>, u64)>) -> Self {
        Self::new_for_test_with_cap(routes, u32::MAX)
    }

    /// Test-only constructor that lets a test set an explicit
    /// `max_failover_attempts` cap (see `new_for_test` docs).
    #[cfg(test)]
    pub(crate) fn new_for_test_with_cap(
        routes: Vec<(String, Arc<dyn LlmProvider>, u64)>,
        max_failover_attempts: u32,
    ) -> Self {
        let entries = routes
            .into_iter()
            .map(|(key, provider, cooldown_secs)| RouteEntry {
                condition: "default".to_string(),
                key,
                provider,
                cooldown_duration: std::time::Duration::from_secs(cooldown_secs.max(1)),
            })
            .collect();
        Self {
            routes: entries,
            cooldowns: std::sync::Mutex::new(std::collections::HashMap::new()),
            max_failover_attempts,
        }
    }
}

// ── Keyword sets for semantic routing ─────────────────────────────────────────

const FINANCIAL_KEYWORDS: &[&str] = &[
    // Russian
    "портфель", "акции", "бумаги", "дивиденды", "доходность", "прибыль", "убыток",
    "imoex", "ртс", "мосбиржа", "moex", "облигации", "фонд", "etf", "паи",
    "котировки", "инвестиц", "брокер", "позиции", "активы", "тикер",
    // English
    "portfolio", "shares", "dividend", "yield", "return", "profit", "loss",
    "stock", "bond", "equity", "ticker", "market",
];

const ANALYTICAL_KEYWORDS: &[&str] = &[
    // Russian
    "анализируй", "подсчитай", "посчитай", "вычисли", "рассчитай", "сравни",
    "корреляция", "среднее", "медиана", "статистика", "динамика", "тренд",
    "процент", "прогноз", "агрегируй", "сгруппируй",
    // English
    "analyze", "calculate", "compute", "correlation", "average", "median",
    "statistics", "trend", "forecast", "aggregate",
];

const CODE_KEYWORDS: &[&str] = &[
    // Russian
    "скрипт", "код", "запусти", "выполни", "python", "bash",
    "напиши скрипт", "напиши код",
    // English
    "script", "code", "execute", "run script", "run code",
];

fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|kw| text.contains(kw))
}

// ── RoutingProvider LlmProvider impl ─────────────────────────────────────────
// NOTE: chat() and chat_stream() have identical routing/fallback logic but
// cannot be unified into a generic helper due to async closure lifetime issues
// with trait objects. The duplication is intentional and kept in sync.

#[async_trait::async_trait]
impl LlmProvider for RoutingProvider {
    async fn chat(
        &self,
        messages: &[hydeclaw_types::Message],
        tools: &[hydeclaw_types::ToolDefinition],
    ) -> Result<hydeclaw_types::LlmResponse> {
        let primary = self.select_route(messages, tools)?;
        let primary_key = primary.key.clone();
        let primary_display = primary.provider.name().to_string();
        let primary_cooldown = primary.cooldown_duration;

        let primary_skipped = self.is_on_cooldown(&primary_key);
        // Most recent failover reason carried from the previous failed
        // route to the next attempt — populated by `handle_provider_error`.
        // The key of the most recent failed route (source of the next
        // failover transition). Starts as `primary_key`; the first
        // transition is recorded as either "primary on cooldown → fallback"
        // or "primary_failed_error → fallback".
        let mut pending_reason: Option<&'static str>;
        let mut last_failed_key = primary_key.clone();

        if primary_skipped {
            tracing::debug!(provider = %primary_display, "primary on cooldown, skipping");
            pending_reason = Some("cooldown");
        } else {
            match primary.provider.chat(messages, tools).await {
                Ok(resp) => return Ok(resp),
                Err(e) => match self.handle_provider_error(&e, &primary_key, primary_cooldown) {
                    None => {
                        // Non-failover-worthy: bubble up with `partial_state` intact.
                        return Err(e);
                    }
                    Some(reason) => {
                        pending_reason = Some(reason);
                    }
                },
            }
        }

        // `pending_reason` is consumed on the next loop iteration; the last
        // iteration's reassignment is by design (dead-store is cheap and
        // keeps the loop body uniform). Silence the unused_assignments lint
        // for the final-iteration dead store on error.
        //
        // Issue #9: enforce `max_failover_attempts` cap — stop iterating
        // once we've attempted N fallbacks, even if more routes remain.
        // `enumerate` is `usize`; we compare against the u32 cap by casting.
        #[allow(unused_assignments)]
        {
            let fallbacks = self.available_fallbacks(&primary_key);
            for (idx, fb) in fallbacks.into_iter().enumerate() {
                if idx as u32 >= self.max_failover_attempts {
                    tracing::warn!(
                        attempts = idx as u32,
                        cap = self.max_failover_attempts,
                        "failover cap reached — not trying further routes"
                    );
                    break;
                }
                // Record failover counter at the transition point.
                if let Some(reason) = pending_reason.take() {
                    Self::record_failover(&last_failed_key, &fb.key, reason);
                }
                tracing::info!(provider = %fb.provider.name(), "trying fallback provider");
                match fb.provider.chat(messages, tools).await {
                    Ok(mut resp) => {
                        let reason = if primary_skipped { "cooldown" } else { "primary_failed" };
                        resp.fallback_notice = Some(format!("↪️ {} → {} ({})", primary_display, fb.provider.name(), reason));
                        return Ok(resp);
                    }
                    Err(e) => {
                        match self.handle_provider_error(&e, &fb.key, fb.cooldown_duration) {
                            None => return Err(e),
                            Some(reason) => {
                                pending_reason = Some(reason);
                                last_failed_key = fb.key.clone();
                            }
                        }
                    }
                }
            }
        }
        anyhow::bail!("all providers failed (including fallbacks)")
    }

    async fn chat_stream(
        &self,
        messages: &[hydeclaw_types::Message],
        tools: &[hydeclaw_types::ToolDefinition],
        chunk_tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<hydeclaw_types::LlmResponse> {
        let primary = self.select_route(messages, tools)?;
        let primary_key = primary.key.clone();
        let primary_display = primary.provider.name().to_string();
        let primary_cooldown = primary.cooldown_duration;

        let primary_skipped = self.is_on_cooldown(&primary_key);
        let mut pending_reason: Option<&'static str>;
        let mut last_failed_key = primary_key.clone();

        if primary_skipped {
            tracing::debug!(provider = %primary_display, "primary on cooldown, skipping for streaming");
            pending_reason = Some("cooldown");
        } else {
            use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
            let chunks_sent = Arc::new(AtomicBool::new(false));
            let (tracking_tx, mut tracking_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let forwarder = {
                let sentinel = chunks_sent.clone();
                let forward_tx = chunk_tx.clone();
                tokio::spawn(async move {
                    while let Some(chunk) = tracking_rx.recv().await {
                        sentinel.store(true, Ordering::Relaxed);
                        forward_tx.send(chunk).ok();
                    }
                })
            };

            match primary.provider.chat_stream(messages, tools, tracking_tx).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    // tracking_tx is now consumed/dropped by the call above.
                    // Wait for the forwarder to drain any buffered chunks before
                    // reading chunks_sent — this eliminates the race condition.
                    let _ = forwarder.await;

                    if chunks_sent.load(Ordering::Relaxed) {
                        // Issue #6: mid-stream failure cannot fail over (user
                        // already received partial content), but the primary
                        // still deserves a cooldown + metric bump so we don't
                        // re-hammer it on the next request. Swallow the
                        // returned "failover reason" — we're not actually
                        // failing over.
                        let _ = self.handle_provider_error(&e, &primary_key, primary_cooldown);
                        tracing::warn!(provider = %primary_display, error = %e,
                            "streaming: mid-stream failure, partial output already sent — cooldown applied, not failing over");
                        return Err(e);
                    }
                    // Downcast to typed error; non-failover-worthy errors bubble up
                    // (preserving `partial_state`); failover-worthy errors apply
                    // cooldown and fall through to the fallback chain.
                    match self.handle_provider_error(&e, &primary_key, primary_cooldown) {
                        None => return Err(e),
                        Some(reason) => {
                            pending_reason = Some(reason);
                        }
                    }
                    tracing::warn!(provider = %primary_display,
                        "streaming: primary failed before first chunk, trying fallback chain");
                }
            }
        }

        // See the `chat` impl for the rationale behind the
        // `unused_assignments` allow — the last iteration's reassignment
        // on error is dead-store by design.
        //
        // Issue #9: enforce `max_failover_attempts` cap, same as `chat()`.
        #[allow(unused_assignments)]
        {
            let fallbacks = self.available_fallbacks(&primary_key);
            for (idx, fb) in fallbacks.into_iter().enumerate() {
                if idx as u32 >= self.max_failover_attempts {
                    tracing::warn!(
                        attempts = idx as u32,
                        cap = self.max_failover_attempts,
                        "streaming failover cap reached — not trying further routes"
                    );
                    break;
                }
                if let Some(reason) = pending_reason.take() {
                    Self::record_failover(&last_failed_key, &fb.key, reason);
                }
                tracing::info!(provider = %fb.provider.name(), "trying streaming fallback provider");
                match fb.provider.chat_stream(messages, tools, chunk_tx.clone()).await {
                    Ok(mut resp) => {
                        let reason = if primary_skipped { "cooldown" } else { "primary_failed" };
                        resp.fallback_notice = Some(format!("↪️ {} → {} ({})", primary_display, fb.provider.name(), reason));
                        return Ok(resp);
                    }
                    Err(e) => {
                        match self.handle_provider_error(&e, &fb.key, fb.cooldown_duration) {
                            None => return Err(e),
                            Some(reason) => {
                                pending_reason = Some(reason);
                                last_failed_key = fb.key.clone();
                            }
                        }
                    }
                }
            }
        }
        anyhow::bail!("all streaming providers failed (including fallbacks)")
    }

    fn name(&self) -> &'static str {
        "routing"
    }

    fn set_model_override(&self, model: Option<String>) {
        for entry in &self.routes {
            entry.provider.set_model_override(model.clone());
        }
    }

    fn current_model(&self) -> String {
        self.routes
            .first().map_or_else(|| "unknown".to_string(), |e| e.provider.current_model())
    }
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
    // Pass 1: collect all tool_call_ids that have a saved tool result.
    let mut result_ids = std::collections::HashSet::<String>::new();
    for msg in messages {
        if msg.role == MessageRole::Tool
            && let Some(ref id) = msg.tool_call_id {
                result_ids.insert(id.clone());
            }
    }

    // Pass 2: rebuild messages, skipping incomplete assistant+tool_calls groups
    // (where some tool results are missing — e.g. process crashed after saving assistant msg).
    let mut valid_call_ids = std::collections::HashSet::<String>::new();
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
                let id = msg.tool_call_id.as_deref().unwrap_or("");
                if valid_call_ids.contains(id) {
                    result.push(msg.clone());
                } else {
                    tracing::warn!(
                        tool_call_id = id,
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
pub(super) fn messages_to_openai_format(messages: &[Message]) -> Vec<serde_json::Value> {
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
                                    "id": tc.id,
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

                        return serde_json::Value::Object(m);
                    }

            m.insert(
                "content".to_string(),
                serde_json::Value::String(msg.content.clone()),
            );

            if let Some(ref tool_call_id) = msg.tool_call_id {
                m.insert(
                    "tool_call_id".to_string(),
                    serde_json::Value::String(tool_call_id.clone()),
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
    use hydeclaw_types::{Message, MessageRole, ToolCall};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn user_msg(content: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        }
    }

    fn assistant_msg(content: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        }
    }

    fn assistant_with_calls(content: &str, calls: Vec<(&str, &str)>) -> Message {
        let tool_calls = calls
            .into_iter()
            .map(|(id, name)| ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: serde_json::json!({}),
            })
            .collect();
        Message {
            role: MessageRole::Assistant,
            content: content.to_string(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            thinking_blocks: vec![],
        }
    }

    fn tool_msg(call_id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
            thinking_blocks: vec![],
        }
    }

    fn system_msg(content: &str) -> Message {
        Message {
            role: MessageRole::System,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
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
        assert_eq!(result.tool_calls[0].id, "call-1");
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
        assert_eq!(result.tool_calls[0].id, "c1");
        assert_eq!(result.tool_calls[1].id, "c2");
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
        assert!(contains_any("write a script", &["script", "code"]));
    }

    #[test]
    fn contains_any_no_match_returns_false() {
        assert!(!contains_any("hello world", &["script", "code", "execute"]));
    }

    #[test]
    fn contains_any_empty_keywords_returns_false() {
        assert!(!contains_any("anything goes here", &[]));
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
        let result = messages_to_openai_format(&msgs);
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
        let result = messages_to_openai_format(&msgs);
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
        let result = messages_to_openai_format(&msgs);
        let tool = &result[2];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["content"], "tool output");
        assert_eq!(tool["tool_call_id"], "call-42");
    }

    #[test]
    fn openai_system_message_preserved() {
        let msgs = vec![system_msg("You are an AI."), user_msg("Hi")];
        let result = messages_to_openai_format(&msgs);
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
        let result = messages_to_openai_format(&msgs);
        let asst = &result[1];
        // non-empty content should be preserved (not null)
        assert_eq!(asst["content"], "Let me search for that.");
        assert!(asst.get("tool_calls").is_some());
    }
}
