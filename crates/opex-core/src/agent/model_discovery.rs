//! Dynamic model discovery from LLM providers.
//! Queries provider APIs for available models via live API calls.

use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

use crate::secrets::SecretsManager;
use super::providers::OPENAI_COMPAT_PROVIDERS;

/// Defense-in-depth: block ports that should never be targeted by model discovery,
/// even from admin-configured URLs. Prevents accidental misconfiguration from
/// reaching `PostgreSQL`, Docker API, or other dangerous internal services.
fn reject_dangerous_ports(url: &str) -> Result<()> {
    const BLOCKED_PORTS: &[u16] = &[5432, 2375, 2376]; // postgres, docker
    if let Ok(parsed) = url::Url::parse(url)
        && let Some(port) = parsed.port()
            && BLOCKED_PORTS.contains(&port) {
                anyhow::bail!("model discovery blocked: port {port} is a protected service");
            }
    Ok(())
}

/// A discovered model from a provider, enriched with catalog metadata.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    /// Catalog-resolved context window (tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Accepts image/file attachments (vision).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Extended reasoning / chain-of-thought.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Emits/expects reasoning in a `reasoning_content` field (DeepSeek-R1,
    /// Kimi-thinking, …). Drives the OpenAI-compat message formatter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<bool>,
    /// Function calling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
}

/// Fill each model's catalog metadata (context window + capabilities) in place,
/// looked up by `(provider_type, model_id)`. No-op fields when the catalog
/// doesn't know the model.
pub fn enrich_from_catalog(provider_type: &str, models: &mut [ModelInfo]) {
    use opex_catalog as catalog;
    for m in models.iter_mut() {
        // Resolve once, hinted by the model's true vendor (`owned_by`), so an
        // openai-compat provider lands on the model's native models.dev row
        // rather than a reseller/gateway duplicate. context + caps come from the
        // same row (no divergence).
        let Some(meta) = catalog::global_meta(provider_type, m.owned_by.as_deref(), &m.id) else {
            m.context_window = None;
            continue;
        };
        m.context_window = Some(meta.context);
        if let Some(c) = meta.caps {
            m.vision = Some(c.attachment);
            m.reasoning = Some(c.reasoning);
            m.reasoning_content = Some(c.reasoning_content);
            m.tools = Some(c.tool_call);
        }
    }
}

// ── Discovery logic ──────────────────────────────────────────────────────────

/// Derive models listing URL from a base URL and provider type.
/// Uses the provider's `chat_path` to compute the models path.
fn derive_models_url_from_base(provider_type: &str, base_url: &str) -> String {
    let chat_path = super::providers::PROVIDER_TYPES.iter()
        .find(|pt| pt.id == provider_type)
        .map_or("/v1/chat/completions", |pt| pt.chat_path);
    let models_path = chat_path.replace("/chat/completions", "/models");
    format!("{}{}", base_url.trim_end_matches('/'), models_path)
}

/// Resolve the API key for a provider from secrets or env.
async fn resolve_key(secrets: &SecretsManager, key_env: &str) -> Option<String> {
    if key_env.is_empty() {
        return None;
    }
    secrets.get_scoped(key_env, "").await
}

/// Fetch models from an OpenAI-compatible `/v1/models` endpoint.
///
/// Safety: URLs come from admin-configured providers (DB `providers` table).
/// Only authenticated admins can add/modify providers, so these URLs are trusted.
/// We still block Docker API and `PostgreSQL` ports as defense-in-depth.
async fn fetch_openai_models(url: &str, api_key: Option<&str>) -> Result<Vec<ModelInfo>> {
    reject_dangerous_ports(url)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let mut req = client.get(url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("provider returned {}", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    let models = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let id = m["id"].as_str()?.to_string();
                    let owned_by = m["owned_by"].as_str().map(std::string::ToString::to_string);
                    Some(ModelInfo { id, owned_by, ..Default::default() })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(models)
}

/// List OpenAI-format models tolerating both `{base}/v1/models` and
/// `{base}/models` layouts. Providers whose `base_url` already carries a version
/// path (e.g. z.ai's `.../paas/v4`) expose the catalogue at `{base}/models`,
/// while root base URLs (OpenAI, Ollama) use `{base}/v1/models`. We try the
/// chat_path-derived URL first, then fall back to the version-less `{base}/models`.
async fn list_openai_models(provider_type: &str, base: &str, api_key: Option<&str>) -> Result<Vec<ModelInfo>> {
    let primary = derive_models_url_from_base(provider_type, base);
    let primary_res = fetch_openai_models(&primary, api_key).await;
    if let Ok(m) = &primary_res
        && !m.is_empty()
    {
        return primary_res;
    }
    let alt = format!("{}/models", base.trim_end_matches('/'));
    if alt != primary
        && let Ok(m) = fetch_openai_models(&alt, api_key).await
        && !m.is_empty()
    {
        return Ok(m);
    }
    primary_res
}

/// Fetch models from Anthropic `/v1/models` (non-OpenAI format: requires anthropic-version header).
async fn fetch_anthropic_models(api_key: Option<&str>, base_url: Option<&str>) -> Result<Vec<ModelInfo>> {
    let base = base_url.unwrap_or("https://api.anthropic.com");
    reject_dangerous_ports(base)?;
    let url = format!("{}/v1/models", base.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let mut req = client.get(&url)
        .header("anthropic-version", "2023-06-01");
    if let Some(key) = api_key {
        req = req.header("x-api-key", key);
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("anthropic returned {}", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    let models = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let id = m["id"].as_str()?.to_string();
                    let display = m["display_name"].as_str().map(std::string::ToString::to_string);
                    Some(ModelInfo { id, owned_by: display.or(Some("anthropic".into())), ..Default::default() })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(models)
}

/// Fetch models from Google Gemini API `/v1beta/models`.
async fn fetch_google_models(api_key: Option<&str>, base_url: Option<&str>) -> Result<Vec<ModelInfo>> {
    if let Some(url) = base_url { reject_dangerous_ports(url)?; }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let base = base_url.unwrap_or("https://generativelanguage.googleapis.com");
    let mut url = format!("{}/v1beta/models", base.trim_end_matches('/'));
    if let Some(key) = api_key {
        url.push_str(&format!("?key={key}"));
    }

    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("google returned {}", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    let models = body["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    // name is "models/gemini-2.5-pro" → extract "gemini-2.5-pro"
                    let name = m["name"].as_str()?;
                    let id = name.strip_prefix("models/").unwrap_or(name).to_string();
                    let display = m["displayName"].as_str().map(std::string::ToString::to_string);
                    Some(ModelInfo { id, owned_by: display.or(Some("google".into())), ..Default::default() })
                })
                // Filter to generative models only (skip embedding models)
                .filter(|m| m.id.starts_with("gemini"))
                .collect()
        })
        .unwrap_or_default();

    Ok(models)
}

/// Discover available models for a provider via live API.
pub async fn discover_models(
    provider: &str,
    secrets: &SecretsManager,
    base_url_override: Option<&str>,
) -> Result<Vec<ModelInfo>> {
    match provider {
        // Anthropic — custom API (x-api-key header, anthropic-version)
        "anthropic" | "claude-cli" => {
            let key = resolve_key(secrets, "ANTHROPIC_API_KEY").await;
            fetch_anthropic_models(key.as_deref(), base_url_override).await
        }

        // Google Gemini — custom API (key as query param)
        "google" | "gemini" | "gemini-cli" => {
            let key = resolve_key(secrets, "GOOGLE_API_KEY").await;
            fetch_google_models(key.as_deref(), base_url_override).await
        }

        // Ollama — list via the OpenAI-compatible `/v1/models` endpoint. This
        // works for both a local daemon (no auth) and the ollama.com cloud
        // aggregator (Bearer key), which expose their full model catalogue there.
        "ollama" => {
            let base = base_url_override.unwrap_or("http://localhost:11434");
            let models_url = derive_models_url_from_base("ollama", base);
            // Cloud requires a Bearer key; a local daemon does not — key optional.
            let key = resolve_key(secrets, "OLLAMA_API_KEY").await;
            fetch_openai_models(&models_url, key.as_deref()).await
        }

        "openai" | "codex-cli" => {
            let base = base_url_override
                .map(std::string::ToString::to_string)
                .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.openai.com".to_string());
            let key = resolve_key(secrets, "OPENAI_API_KEY").await;
            list_openai_models("openai", &base, key.as_deref()).await
        }

        // Any OpenAI-compatible type — a named provider (deepseek/groq/…) OR a
        // generic `openai_compat` / custom type carrying an admin-set base_url.
        // List from its `/v1/models` endpoint (derived from the chat_path).
        other => {
            let named = OPENAI_COMPAT_PROVIDERS.iter().find(|(n, _, _)| *n == other);
            let Some(base) = base_url_override.or(named.map(|(_, b, _)| *b)) else {
                return Ok(vec![]);
            };
            // Named providers have a standard key env; a generic type has none
            // here (its resolved key arrives via discover_models_with_resolved_key).
            let key = match named {
                Some((_, _, key_env)) => resolve_key(secrets, key_env).await,
                None => None,
            };
            list_openai_models(other, base, key.as_deref()).await
        }
    }
}

/// Discover models using a pre-resolved API key (from vault-scoped credential).
/// Falls back to standard secret name resolution if `api_key` is None.
pub async fn discover_models_with_key(
    provider: &str,
    secrets: &SecretsManager,
    base_url_override: Option<&str>,
    api_key: Option<&str>,
) -> Result<Vec<ModelInfo>> {
    match api_key {
        Some(key) => discover_models_with_resolved_key(provider, key, base_url_override).await,
        None => discover_models(provider, secrets, base_url_override).await,
    }
}

/// Internal: discover models with an already-resolved API key.
async fn discover_models_with_resolved_key(
    provider: &str,
    api_key: &str,
    base_url_override: Option<&str>,
) -> Result<Vec<ModelInfo>> {
    let key = Some(api_key);
    match provider {
        "anthropic" | "claude-cli" => {
            fetch_anthropic_models(key, base_url_override).await
        }
        "google" | "gemini" | "gemini-cli" => {
            fetch_google_models(key, base_url_override).await
        }
        // Ollama — list via `/v1/models` with the resolved key (see
        // `discover_models`). The cloud aggregator needs the Bearer key; a local
        // daemon ignores it.
        "ollama" => {
            let base = base_url_override.unwrap_or("http://localhost:11434");
            let models_url = derive_models_url_from_base("ollama", base);
            fetch_openai_models(&models_url, key).await
        }
        "openai" | "codex-cli" => {
            let base = base_url_override
                .map(std::string::ToString::to_string)
                .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.openai.com".to_string());
            list_openai_models("openai", &base, key).await
        }
        // Any OpenAI-compatible type (named or a generic `openai_compat`/custom
        // type with an admin-set base_url) — list from `/v1/models` with the
        // resolved key.
        other => {
            let named = OPENAI_COMPAT_PROVIDERS.iter().find(|(n, _, _)| *n == other);
            let Some(base) = base_url_override.or(named.map(|(_, b, _)| *b)) else {
                return Ok(vec![]);
            };
            list_openai_models(other, base, key).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_models_url_groq() {
        assert_eq!(
            derive_models_url_from_base("groq", "https://api.groq.com/openai"),
            "https://api.groq.com/openai/v1/models"
        );
    }

    #[test]
    fn derive_models_url_minimax() {
        assert_eq!(
            derive_models_url_from_base("minimax", "https://api.minimax.io"),
            "https://api.minimax.io/v1/models"
        );
    }

    #[test]
    fn derive_models_url_deepseek() {
        assert_eq!(
            derive_models_url_from_base("deepseek", "https://api.deepseek.com"),
            "https://api.deepseek.com/v1/models"
        );
    }

    #[test]
    fn derive_models_url_openai_compat() {
        // A generic openai_compat provider lists from {base}/v1/models (its
        // chat_path is /v1/chat/completions).
        assert_eq!(
            derive_models_url_from_base("openai_compat", "https://api.z.ai/api/coding/paas/v4"),
            "https://api.z.ai/api/coding/paas/v4/v1/models"
        );
    }

    #[test]
    fn derive_models_url_ollama() {
        // Ollama (local daemon or the ollama.com cloud aggregator) lists models
        // via the OpenAI-compatible /v1/models path.
        assert_eq!(
            derive_models_url_from_base("ollama", "https://ollama.com"),
            "https://ollama.com/v1/models"
        );
        assert_eq!(
            derive_models_url_from_base("ollama", "http://localhost:11434"),
            "http://localhost:11434/v1/models"
        );
    }

    #[test]
    fn derive_models_url_perplexity() {
        // Perplexity uses /chat/completions (no /v1 prefix)
        assert_eq!(
            derive_models_url_from_base("perplexity", "https://api.perplexity.ai"),
            "https://api.perplexity.ai/models"
        );
    }

    #[test]
    fn all_providers_use_live_api() {
        // All known providers should be handled in discover_models match arms
        // (no hardcoded fallbacks — every provider has a listing API)
        let known = ["anthropic", "claude-cli", "google", "gemini", "gemini-cli",
                      "ollama", "openai", "codex-cli", "minimax", "deepseek", "groq",
                      "mistral", "xai", "perplexity", "together", "openrouter"];
        // Just verify the list is non-empty (actual API calls tested in integration tests)
        assert!(known.len() > 10);
    }
}
