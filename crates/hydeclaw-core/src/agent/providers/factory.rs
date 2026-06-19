//! Provider factories: turn a `ProviderRow` (or agent-config) into an
//! `Arc<dyn LlmProvider>`.
//!
//! Three entry points:
//!
//! - [`build_provider`] — sync constructor for HTTP providers (anthropic,
//!   google, openai-compatible). Dispatches on `row.provider_type` to the
//!   right `*_impl::*::new_from_row`.
//! - [`build_cli_provider`] — async constructor for CLI providers
//!   (`claude-cli`, `gemini-cli`, `codex-cli`). Needs a [`CliContext`]
//!   carrying sandbox + agent-name + workspace path.
//! - [`resolve_provider_for_agent`] — high-level: takes an
//!   `AgentSettings`, looks up the named connection in the DB, and
//!   delegates to the right builder. Falls back to
//!   [`UnconfiguredProvider`] on any failure so calls surface a typed
//!   error instead of panicking.
//!
//! All HTTP providers share `build_provider_clients(&timeouts)` for the
//! request + streaming `reqwest::Client` pair.

use std::sync::Arc;

use crate::secrets::SecretsManager;

use super::timeouts::{self, TimeoutsConfig};
use super::{
    AnthropicProvider, ClaudeCliProvider, GoogleProvider, LlmProvider,
    OpenAiCompatibleProvider, UnconfiguredProvider,
};

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
        #[cfg(feature = "gemini-cloudcode")]
        "gemini-cloudcode" => {
            let provider =
                super::gemini_cloudcode::provider::GeminiCloudCodeProvider::new_from_row(
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
    let config = crate::agent::cli_backend::resolve_cli_config(preset_id, db_options)?;
    Some(Arc::new(ClaudeCliProvider::new(
        preset_id, config, model.to_string(), sandbox, agent_name.to_string(), workspace_dir.to_string(), base, secrets, api_key,
    )))
}

/// Resolve LLM provider for an agent from a named connection in the DB.
/// The agent MUST have `provider_connection` set.
///
/// Returns a sentinel "unconfigured" provider if no usable connection is found.
/// No free-form `provider`-field fallback — agents without a valid
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
                    agent.prompt_cache,
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
    agent_prompt_cache: bool,
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
            // CACHE-01: thread agent TOML `prompt_cache` into the override chain.
            // `Some(false)` is explicit and overrides any `prompt_cache: true` in the
            // provider's `options` JSON — agent-level config wins (Pitfall 3 in 68-RESEARCH).
            // Anthropic-only effect; non-Anthropic providers ignore this field (CACHE-04).
            // The routing path (`routing::create_routing_provider`) propagates the
            // same flag, so multi-route agents get the same cache behaviour.
            let overrides = ProviderOverrides {
                model: model_override.map(str::to_string),
                temperature: Some(temperature),
                max_tokens,
                prompt_cache: Some(agent_prompt_cache),
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
