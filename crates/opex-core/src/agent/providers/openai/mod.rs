//! OpenAI-compatible LLM provider (`MiniMax`, `OpenAI`, Ollama, etc.) —
//! extracted from providers.rs for readability.

use super::{async_trait, Arc, SecretsManager, ModelOverride, LlmProvider, Message, ToolDefinition, Result, LlmResponse, messages_to_openai_format, mpsc, HttpTransport};

mod chat;
mod chat_stream;
mod minimax_xml;
mod request;
mod response;
mod stream;

// ── OpenAI-Compatible Provider (works with MiniMax, OpenAI, Ollama, etc.) ──

pub struct OpenAiCompatibleProvider {
    provider_name: String,
    client: Arc<dyn HttpTransport>,
    streaming_client: Arc<dyn HttpTransport>,
    url: String,
    /// Raw base URL (before appending chat path), used for provider-specific API calls
    /// such as Ollama's /api/show for context-limit discovery.
    api_base_url: String,
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
    /// Max HTTP retry attempts on transient errors (429/5xx). Configurable per-provider via UI.
    max_retries: u32,
    /// Operator-configured per-model context windows (tokens) from
    /// `providers.options.context_windows`. Highest-priority source in
    /// `context_limit_hint` — set entries for models whose API doesn't expose
    /// the window (e.g. MiMo).
    context_windows: Option<std::collections::BTreeMap<String, u32>>,
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
            api_base_url: base,
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
            max_retries: opts.max_retries,
            context_windows: opts.context_windows,
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
    pub(super) fn with_credential_scope(mut self, scope: String) -> Self {
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
    pub(super) fn with_keys(mut self, api_key_names: Vec<String>) -> Self {
        self.api_key_names = api_key_names;
        self
    }

    /// Set dynamic base URL resolution from secrets (e.g. "`OLLAMA_URL`").
    /// On each LLM call, resolves the secret and appends `suffix` to form the full URL.
    /// Dead code — kept as example for future secret-backed URL resolution.
    #[allow(dead_code)]
    pub(super) fn with_base_url_env(mut self, env_name: &str, suffix: &str) -> Self {
        self.base_url_env = Some(env_name.to_string());
        self.url_suffix = suffix.to_string();
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

    fn context_size(messages: &[Message]) -> usize {
        messages.iter().map(|m| {
            m.content.len()
                + m.tool_calls.as_deref().unwrap_or(&[]).iter()
                    .map(|tc| serde_json::to_string(&tc.arguments).map(|s| s.len()).unwrap_or(0))
                    .sum::<usize>()
        }).sum()
    }

    /// Whether the model emits/expects assistant reasoning in a `reasoning_content`
    /// field (DeepSeek-R1, Kimi-thinking, …). Catalog-driven when the model is
    /// known (models.dev `interleaved.field == "reasoning_content"` — accurate
    /// per-model: deepseek-reasoner yes, deepseek-chat no); name-match fallback
    /// for uncatalogued models.
    fn uses_reasoning_content(&self) -> bool {
        let model = self.model.effective();
        if let Some(rc) = opex_catalog::global_caps(&self.provider_name, &model).map(|c| c.reasoning_content) {
            return rc;
        }
        self.provider_name == "deepseek" || model.to_lowercase().contains("deepseek")
    }

    fn supports_forced_tool_choice(&self) -> bool {
        let m = self.model.effective().to_lowercase();
        !(m.contains("reasoner")
            || m.contains("v4-pro")
            || m.starts_with("deepseek-r1")
            || m.contains("/r1")
            || m.ends_with("-r1"))
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
        opts: super::CallOptions,
    ) -> Result<LlmResponse> {
        self.execute_chat(messages, tools, opts).await
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::Sender<String>,
        opts: super::CallOptions,
    ) -> Result<LlmResponse> {
        self.execute_chat_stream(messages, tools, chunk_tx, opts).await
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

    async fn context_limit_hint(&self, model: &str) -> Option<u32> {
        // Operator-configured per-model window wins — for providers whose API
        // doesn't expose it (e.g. MiMo's /v1/models returns no context field,
        // leaving the model on the 128k name-heuristic fallback).
        if let Some(w) = self.context_windows.as_ref().and_then(|m| m.get(model)) {
            return Some(*w);
        }
        if self.provider_name == "ollama" {
            self.ollama_context_limit(model).await
        } else {
            self.openai_compat_context_limit(model).await
        }
    }
}

impl OpenAiCompatibleProvider {
    /// Query Ollama's /api/show for the real context window.
    async fn ollama_context_limit(&self, model: &str) -> Option<u32> {
        let url = format!("{}/api/show", self.api_base_url.trim_end_matches('/'));
        let resp = self.client.discovery_client()
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .json(&serde_json::json!({ "model": model }))
            .send().await.ok()?
            .error_for_status().ok()?
            .json::<serde_json::Value>().await.ok()?;

        // model_info: any key ending in ".context_length"
        if let Some(info) = resp.get("model_info").and_then(|v| v.as_object()) {
            for (key, val) in info {
                if key.ends_with(".context_length")
                    && let Some(n) = val.as_u64() {
                        tracing::debug!(model, context_length = n, "ollama /api/show context_length");
                        return Some(n as u32);
                    }
            }
        }
        // fallback: parse "parameters" field for "num_ctx N"
        if let Some(params) = resp.get("parameters").and_then(|v| v.as_str()) {
            for line in params.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 2 && parts[0] == "num_ctx"
                    && let Ok(n) = parts[1].parse::<u32>() {
                        tracing::debug!(model, num_ctx = n, "ollama num_ctx parameter");
                        return Some(n);
                    }
            }
        }
        None
    }

    /// Query OpenAI-compatible /v1/models to find context window for this model.
    ///
    /// Tries the single-model endpoint first (`GET /v1/models/{id}`), then falls
    /// back to the full list (`GET /v1/models`) and filters by id.
    /// Checks multiple field names used across different providers:
    /// `context_window` (OpenAI/Groq), `context_length` (OpenRouter/Together),
    /// `max_context_length` (Mistral), `max_model_len` (vLLM), `max_seq_len` (SGLang).
    async fn openai_compat_context_limit(&self, model: &str) -> Option<u32> {
        let base = self.api_base_url.trim_end_matches('/');
        let api_key = self.resolve_api_key().await;

        let auth = |req: reqwest::RequestBuilder| {
            if api_key.is_empty() { req } else { req.bearer_auth(&api_key) }
        };

        // Helper: extract context limit from a model JSON object.
        let extract = |obj: &serde_json::Value| -> Option<u32> {
            let n = obj.get("context_window")
                .or_else(|| obj.get("context_length"))
                .or_else(|| obj.get("max_context_length"))
                .or_else(|| obj.get("max_model_len"))
                .or_else(|| obj.get("max_seq_len"))
                .and_then(|v| v.as_u64())?;
            Some(n as u32)
        };

        // 1. Try individual model endpoint (faster, lower bandwidth).
        let single_url = format!("{}/v1/models/{}", base, model);
        if let Ok(resp) = auth(self.client.discovery_client().get(&single_url).timeout(std::time::Duration::from_secs(5)))
            .send().await
            && let Ok(resp) = resp.error_for_status()
                && let Ok(obj) = resp.json::<serde_json::Value>().await
                    && let Some(n) = extract(&obj) {
                        tracing::debug!(model, context = n, "openai-compat /v1/models/{model}");
                        return Some(n);
                    }

        // 2. Fall back to full list and filter by id.
        let list_url = format!("{}/v1/models", base);
        let resp = auth(self.client.discovery_client().get(&list_url).timeout(std::time::Duration::from_secs(5)))
            .send().await.ok()?
            .error_for_status().ok()?
            .json::<serde_json::Value>().await.ok()?;

        let models = resp.get("data").and_then(|v| v.as_array())?;
        for m in models {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            if id == model
                && let Some(n) = extract(m) {
                    tracing::debug!(model, context = n, "openai-compat /v1/models list");
                    return Some(n);
                }
        }
        None
    }
}

#[cfg(test)]
mod golden_fixtures {
    // Reserved for future OpenAI-related golden-fixture tests. The original
    // MiniMax XML fixtures moved to `minimax_xml::golden_fixtures` along with
    // the production code in W1 Task 9.
}
