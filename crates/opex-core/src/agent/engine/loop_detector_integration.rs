//! `tool_loop_config` + `create_fallback_provider` + LLM-call wrappers for
//! the fallback/retry path through the loop detector and budget guard.

use std::sync::Arc;

use anyhow::Result;
use opex_types::{Message, ToolDefinition};
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

    /// Build the next usable fallback provider starting the chain-walk at
    /// `chain_idx` (`chain_idx=0` → `text[1]`, `chain_idx=1` → `text[2]`, …),
    /// SKIPPING any reserve currently cooled (Session Resilience Task 4 /
    /// WS4 — `crate::agent::provider_cooldown::resolve_next_uncooled`).
    /// Returns `(provider, resolved_idx)` — `resolved_idx` is the chain
    /// position actually adopted, which the caller must feed back into
    /// `LayerRuntimeState::fallback_chain_idx` BEFORE its own `adopt_fallback`
    /// `+1` bump, so the next failover resumes past this entry rather than
    /// re-examining it.
    ///
    /// `None` when the chain is exhausted, or every remaining reserve from
    /// `chain_idx` onward is cooled.
    ///
    /// The primary provider is `text[0]`, already live as `cfg().provider`, so
    /// reserves start at index `1 + chain_idx`. The reserve's per-slot `model`
    /// (`SlotEntry.model`) is honored when present.
    ///
    /// `pub(crate)` so `pipeline::execute` can engage the fallback layer
    /// (`BehaviourLayers::fallback_provider`) without going through the
    /// engine's private API. The legacy `handle_isolated` caller is in
    /// the same module and used `pub(super)`; widening visibility is
    /// safe — every caller is still inside `opex-core`.
    pub(crate) async fn create_fallback_provider(
        &self,
        chain_idx: usize,
    ) -> Option<(Arc<dyn crate::agent::providers::LlmProvider>, usize)> {
        let chain = self.cfg().profile_slots.get("text")?;
        let (entry, resolved_idx) = crate::agent::provider_cooldown::resolve_next_uncooled(
            chain,
            &self.cfg().cooldowns,
            chain_idx,
        )?;
        if resolved_idx != chain_idx {
            tracing::info!(
                agent = %self.cfg().agent.name,
                from_idx = chain_idx,
                to_idx = resolved_idx,
                provider = %entry.provider,
                "provider_cooldown_skip: skipped cooled reserve(s) walking the fallback chain"
            );
        }
        let provider = crate::agent::pipeline::llm_call::create_fallback_provider(
            &self.cfg().db,
            Some(entry.provider.as_str()),
            entry.model.as_deref(),
            &self.cfg().agent.name,
            self.cfg().agent.temperature,
            self.cfg().agent.max_tokens,
            self.secrets().clone(),
            self.sandbox().clone(),
            &self.cfg().workspace_dir,
            self.cfg().agent.base,
        )
        .await;
        provider.map(|p| (p, resolved_idx))
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
    ) -> Result<opex_types::LlmResponse> {
        self.check_budget().await?;
        crate::agent::pipeline::llm_call::chat_with_overflow_recovery(
            self.cfg().provider.as_ref(),
            messages,
            tools,
            self,
        )
        .await
    }

    // ── OpenAI-compatible API handler ───────────────────────────────────────

    pub async fn handle_openai(
        &self,
        openai_messages: &[crate::gateway::OpenAiMessage],
        chunk_tx: Option<mpsc::Sender<String>>,
    ) -> Result<opex_types::LlmResponse> {
        let ctx = crate::agent::pipeline::CommandContext { cfg: self.cfg(), state: self.state(), tex: self.tex(), subagent_depth: 0 };
        crate::agent::pipeline::openai_compat::handle_openai(&ctx, self, openai_messages, chunk_tx).await
    }
}
