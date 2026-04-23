//! REF-01 Task 7: `tool_loop_config` + `create_fallback_provider` + the
//! LLM-call wrappers that drive the fallback/retry path through the loop
//! detector + budget guard.
//!
//! Extracted from `engine/mod.rs` as part of plan 66-02. After Task 7
//! `engine/mod.rs` is a thin dispatcher (< 600 lines).

use std::sync::Arc;

use anyhow::Result;
use hydeclaw_types::{Message, ToolDefinition};
use tokio::sync::mpsc;

use super::AgentEngine;

impl AgentEngine {
    /// Build tool loop config from agent TOML settings (or defaults).
    pub(crate) fn tool_loop_config(&self) -> crate::agent::tool_loop::ToolLoopConfig {
        self.cfg().agent
            .tool_loop
            .as_ref()
            .map(crate::agent::tool_loop::ToolLoopConfig::from)
            .unwrap_or_default()
    }

    /// Create fallback LLM provider from agent config.
    pub(super) async fn create_fallback_provider(&self) -> Option<Arc<dyn crate::agent::providers::LlmProvider>> {
        crate::agent::pipeline::llm_call::create_fallback_provider(
            &self.cfg().db,
            self.cfg().agent.fallback_provider.as_deref(),
            &self.cfg().agent.name,
            self.cfg().agent.temperature,
            self.cfg().agent.max_tokens,
            self.secrets().clone(),
            self.sandbox().clone(),
            &self.cfg().workspace_dir,
            self.cfg().agent.base,
        )
        .await
    }

    /// Check daily token budget before LLM call.
    pub(super) async fn check_budget(&self) -> Result<()> {
        crate::agent::pipeline::llm_call::check_budget(
            &self.cfg().db,
            &self.cfg().agent.name,
            self.cfg().agent.daily_budget_tokens,
        )
        .await
    }

    /// Call LLM with automatic context overflow recovery.
    pub(crate) async fn chat_with_overflow_recovery(
        &self,
        messages: &mut Vec<Message>,
        tools: &[ToolDefinition],
    ) -> Result<hydeclaw_types::LlmResponse> {
        self.check_budget().await?;
        crate::agent::pipeline::llm_call::chat_with_overflow_recovery(
            self.cfg().provider.as_ref(),
            messages,
            tools,
            self,
        )
        .await
    }

    /// Call LLM with exponential backoff retry.
    pub(super) async fn chat_with_transient_retry(
        &self,
        messages: &mut Vec<Message>,
        tools: &[ToolDefinition],
    ) -> Result<hydeclaw_types::LlmResponse> {
        self.check_budget().await?;
        let start = std::time::Instant::now();
        let result = crate::agent::pipeline::llm_call::chat_with_transient_retry(
            self.cfg().provider.as_ref(),
            messages,
            tools,
            self,
        )
        .await;
        self.record_llm_metrics(self.cfg().provider.as_ref(), start.elapsed(), result.as_ref());
        result
    }

    /// Streaming variant of chat_with_overflow_recovery.
    #[allow(dead_code)]
    pub(super) async fn chat_stream_with_overflow_recovery(
        &self,
        messages: &mut Vec<Message>,
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
    ) -> Result<hydeclaw_types::LlmResponse> {
        self.check_budget().await?;
        crate::agent::pipeline::llm_call::chat_stream_with_overflow_recovery(
            self.cfg().provider.as_ref(),
            messages,
            tools,
            chunk_tx,
            self,
        )
        .await
    }

    /// Streaming variant of chat_with_transient_retry.
    #[allow(dead_code)]
    pub(super) async fn chat_stream_with_transient_retry(
        &self,
        messages: &mut Vec<Message>,
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
    ) -> Result<hydeclaw_types::LlmResponse> {
        self.check_budget().await?;
        let start = std::time::Instant::now();
        let result = crate::agent::pipeline::llm_call::chat_stream_with_transient_retry(
            self.cfg().provider.as_ref(),
            messages,
            tools,
            chunk_tx,
            self,
        )
        .await;
        self.record_llm_metrics(self.cfg().provider.as_ref(), start.elapsed(), result.as_ref());
        result
    }

    /// Variant that uses an explicit provider (for fallback switching).
    pub(super) async fn chat_with_transient_retry_using(
        &self,
        provider: &Arc<dyn crate::agent::providers::LlmProvider>,
        messages: &mut Vec<Message>,
        tools: &[ToolDefinition],
    ) -> Result<hydeclaw_types::LlmResponse> {
        self.check_budget().await?;
        let start = std::time::Instant::now();
        let result = crate::agent::pipeline::llm_call::chat_with_transient_retry_using(
            provider,
            messages,
            tools,
            self,
        )
        .await;
        self.record_llm_metrics(provider.as_ref(), start.elapsed(), result.as_ref());
        result
    }

    /// Streaming variant of chat_with_transient_retry_using.
    #[allow(dead_code)]
    pub(super) async fn chat_stream_with_transient_retry_using(
        &self,
        provider: &Arc<dyn crate::agent::providers::LlmProvider>,
        messages: &mut Vec<Message>,
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
    ) -> Result<hydeclaw_types::LlmResponse> {
        self.check_budget().await?;
        let start = std::time::Instant::now();
        let result = crate::agent::pipeline::llm_call::chat_stream_with_transient_retry_using(
            provider,
            messages,
            tools,
            chunk_tx,
            self,
        )
        .await;
        self.record_llm_metrics(provider.as_ref(), start.elapsed(), result.as_ref());
        result
    }

    /// Phase 65 OBS-02: record `llm_call_duration_seconds` + `llm_tokens_total`
    /// after an LLM call (streaming or non-streaming). `provider` / `model` are
    /// bounded-cardinality (provider registry + model override), `result` is
    /// "ok" / "error". Token counts come from `LlmResponse.usage` when present.
    fn record_llm_metrics(
        &self,
        provider: &dyn crate::agent::providers::LlmProvider,
        elapsed: std::time::Duration,
        result: std::result::Result<&hydeclaw_types::LlmResponse, &anyhow::Error>,
    ) {
        let result_label = if result.is_ok() { "ok" } else { "error" };
        let provider_name = self.cfg().agent.provider.as_str();
        let model = provider.current_model();
        self.cfg()
            .metrics
            .record_llm_call_duration(provider_name, &model, result_label, elapsed);

        if let Ok(resp) = result
            && let Some(usage) = resp.usage.as_ref()
        {
            if usage.input_tokens > 0 {
                self.cfg()
                    .metrics
                    .record_llm_tokens(u64::from(usage.input_tokens), "prompt");
            }
            if usage.output_tokens > 0 {
                self.cfg()
                    .metrics
                    .record_llm_tokens(u64::from(usage.output_tokens), "completion");
            }
        }
    }

    /// Fire-and-forget audit event recording.
    #[allow(dead_code)]
    pub(super) fn audit(&self, event_type: &'static str, actor: Option<&str>, details: serde_json::Value) {
        crate::agent::pipeline::llm_call::audit(
            self.cfg().db.clone(),
            self.cfg().agent.name.clone(),
            event_type,
            actor,
            details,
        );
    }

    // ── OpenAI-compatible API handler ───────────────────────────────────────

    pub async fn handle_openai(
        &self,
        openai_messages: &[crate::gateway::OpenAiMessage],
        chunk_tx: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<hydeclaw_types::LlmResponse> {
        let ctx = crate::agent::pipeline::CommandContext { cfg: self.cfg(), state: self.state(), tex: self.tex() };
        crate::agent::pipeline::openai_compat::handle_openai(&ctx, self, openai_messages, chunk_tx).await
    }
}
