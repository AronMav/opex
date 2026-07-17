//! Generic CLI LLM provider — used for Claude CLI, Gemini CLI, and other CLI backends.

use super::{async_trait, LlmProvider, Message, ToolDefinition, Result, LlmResponse};
use std::sync::Arc;
use crate::agent::cli_backend::{CliBackendConfig, CliRunner, format_messages_for_cli};

/// Generic CLI-based LLM provider. Wraps `CliRunner` with a provider name.
/// API key resolution order: direct `api_key` (from provider record) → vault by `env_key` → parent env.
pub struct CliLlmProvider {
    runner: Arc<CliRunner>,
    provider_name: String,
    model: String,
    sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    agent_name: String,
    workspace_dir: String,
    base: bool,
    secrets: Arc<crate::secrets::SecretsManager>,
    env_key: Option<String>,
    /// Direct API key from provider record (`providers.api_key` column).
    api_key: Option<String>,
}

impl CliLlmProvider {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_name: &str,
        config: CliBackendConfig,
        model: String,
        sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
        agent_name: String,
        workspace_dir: String,
        base: bool,
        secrets: Arc<crate::secrets::SecretsManager>,
        api_key: Option<String>,
    ) -> Self {
        let env_key = config.env_key.clone();
        Self {
            runner: Arc::new(CliRunner::new(config)),
            provider_name: provider_name.to_string(),
            model, sandbox, agent_name, workspace_dir, base,
            secrets,
            env_key,
            api_key,
        }
    }

    /// Build a `CliLlmProvider` from a `ProviderRow` + runtime CLI context.
    /// Delegates to `::new(..)` so behavior is identical to `create_provider_from_connection`.
    #[allow(dead_code)] // consumed by super::build_cli_provider
    pub(crate) async fn new_from_row(
        row: &crate::db::providers::ProviderRow,
        model_override: Option<&str>,
        ctx: super::CliContext<'_>,
    ) -> anyhow::Result<Self> {
        let config = crate::agent::cli_backend::resolve_cli_config(&row.provider_type, &row.options)
            .ok_or_else(|| anyhow::anyhow!("unknown CLI preset: {}", row.provider_type))?;
        let model = model_override
            .map(str::to_string)
            .or_else(|| row.default_model.clone())
            .unwrap_or_default();

        // Resolve API key from vault scoped by provider UUID
        let key_env = config.env_key.clone();
        let api_key = if let Some(ref k) = key_env {
            ctx.secrets.get_scoped(k, &row.id.to_string()).await
        } else {
            None
        };

        Ok(Self::new(
            &row.provider_type,
            config,
            model,
            ctx.sandbox,
            ctx.agent_name.to_string(),
            ctx.workspace_dir.to_string(),
            ctx.base,
            ctx.secrets,
            api_key,
        ))
    }

    /// Wave-2 Task 12: resolve the model to pass as the CLI's model
    /// argument for this turn — a per-turn `CallOptions.model_override` wins
    /// over the provider's configured model. Never mutates `self.model`.
    /// Extracted as a pure method (no `&self.runner` spawn) so the
    /// precedence is unit-testable without running a real CLI subprocess.
    fn effective_model<'a>(&'a self, opts: &'a super::CallOptions) -> &'a str {
        opts.model_override.as_deref().unwrap_or(&self.model)
    }
}

#[async_trait]
impl LlmProvider for CliLlmProvider {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        opts: super::CallOptions,
    ) -> Result<LlmResponse> {
        let (prompt, system) = format_messages_for_cli(messages);

        // Honored here (unlike a pure HTTP body) because the model is just a
        // `&str` argument passed to the CLI runner, not baked into a fixed
        // request — see `effective_model()` doc comment.
        let effective_model = self.effective_model(&opts);

        // Resolve API key: provider record → vault → parent env (inherited)
        let mut extra_env = std::collections::HashMap::new();
        if let Some(ref key_name) = self.env_key {
            let resolved = if let Some(ref direct_key) = self.api_key {
                Some(direct_key.clone())
            } else {
                self.secrets.get(key_name).await
            };
            if let Some(key_value) = resolved {
                extra_env.insert(key_name.clone(), key_value);
            }
        }

        // Compute context hash (system prompt + API key) for session invalidation
        {
            use std::hash::{Hash, Hasher};
            use std::collections::hash_map::DefaultHasher;

            let mut hasher = DefaultHasher::new();
            if let Some(ref sp) = system {
                sp.hash(&mut hasher);
            }
            if let Some(ref key_name) = self.env_key
                && let Some(ref key_value) = extra_env.get(key_name) {
                    key_value.hash(&mut hasher);
                }
            let context_hash = hasher.finish();
            self.runner.check_and_invalidate_session(&self.agent_name, context_hash).await;
        }

        let result = self.runner.run(
            &self.agent_name,
            &prompt,
            system.as_deref(),
            effective_model,
            self.sandbox.as_deref(),
            &self.workspace_dir,
            self.base,
            &extra_env,
        ).await?;

        Ok(LlmResponse {
            content: result.text,
            tool_calls: vec![],
            usage: result.usage,
            finish_reason: None,
            model: Some(format!("{}/{}", self.provider_name, effective_model)),
            provider: Some(self.provider_name.clone()),
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        })
    }

    fn name(&self) -> &str { &self.provider_name }
    fn current_model(&self) -> String { self.model.clone() }
}

// Type aliases for backward compatibility
pub type ClaudeCliProvider = CliLlmProvider;

// ── Wave-2 Task 12: per-turn model override tests ────────────────────────────

#[cfg(test)]
mod model_override_tests {
    use super::*;

    fn test_provider(default_model: &str) -> CliLlmProvider {
        // Minimal config — only `command` is required, everything else has
        // a serde default. `chat()` is never invoked by these tests, so no
        // real CLI subprocess is spawned.
        let config: CliBackendConfig =
            serde_json::from_value(serde_json::json!({"command": "true"})).unwrap();
        CliLlmProvider::new(
            "claude-cli",
            config,
            default_model.to_string(),
            None,
            "test-agent".to_string(),
            "/tmp/workspace".to_string(),
            false,
            Arc::new(crate::secrets::SecretsManager::new_noop()),
            None,
        )
    }

    // NOTE: `#[tokio::test]` (not plain `#[test]`) — `SecretsManager::new_noop()`
    // builds a lazy sqlx pool that requires an active Tokio runtime context to
    // construct, even though nothing here actually awaits.

    #[tokio::test]
    async fn model_override_replaces_configured_model() {
        let provider = test_provider("claude-opus-4-6");
        let opts = super::super::CallOptions {
            model_override: Some("test-override-model".to_string()),
            ..Default::default()
        };
        assert_eq!(provider.effective_model(&opts), "test-override-model");
    }

    #[tokio::test]
    async fn no_override_uses_configured_model() {
        let provider = test_provider("claude-opus-4-6");
        assert_eq!(
            provider.effective_model(&super::super::CallOptions::default()),
            "claude-opus-4-6"
        );
    }

    #[tokio::test]
    async fn model_override_does_not_mutate_provider_state() {
        // Non-persistence guarantee: resolving an override must not touch
        // `self.model` — `current_model()` must be unaffected.
        let provider = test_provider("claude-opus-4-6");
        let opts = super::super::CallOptions {
            model_override: Some("test-override-model".to_string()),
            ..Default::default()
        };
        let _ = provider.effective_model(&opts);
        assert_eq!(provider.current_model(), "claude-opus-4-6");
    }
}
