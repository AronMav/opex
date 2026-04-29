//! REF-01 Task 6: trait impls that wire `AgentEngine` into the tool pipeline.
//!
//! Owns:
//! - `impl ToolExecutorDeps for AgentEngine`
//! - `impl pipeline::parallel::ToolExecutor for AgentEngine`
//! - `impl pipeline::llm_call::Compactor for AgentEngine`
//! - inherent methods: `tool_groups`, `internal_tool_definitions[_for_subagent]`,
//!   `execute_tool_calls_partitioned`
//! - the top-level `all_system_tool_names()` accessor (re-exported from
//!   `engine/mod.rs` via `pub use`)
//!
//! Extracted from `engine/mod.rs` as part of plan 66-02.

use anyhow::Result;
use hydeclaw_types::{Message, ToolDefinition};
use uuid::Uuid;

use super::{AgentEngine, LoopBreak};
use crate::agent::tool_loop::LoopDetector;

impl AgentEngine {
    // ── Tool definitions (from engine_tool_defs.rs) ──────────────────────────

    /// Resolve tool group settings (from agent config or defaults).
    pub(super) fn tool_groups(&self) -> &crate::config::ToolGroups {
        crate::agent::pipeline::tool_defs::resolve_tool_groups(self.cfg().agent.tools.as_ref())
    }

    /// Return tool definitions for internal tools available to the LLM.
    pub(super) fn internal_tool_definitions(&self) -> Vec<ToolDefinition> {
        let browser_url = crate::agent::pipeline::canvas::browser_renderer_url();
        let ctx = crate::agent::pipeline::tool_defs::ToolDefsContext {
            is_base: self.cfg().agent.base,
            groups: self.tool_groups(),
            default_timezone: &self.cfg().default_timezone,
            has_sandbox: self.sandbox().is_some(),
            browser_renderer_url: &browser_url,
        };
        crate::agent::pipeline::tool_defs::build_internal_tool_definitions(&ctx)
    }

    /// Internal tool definitions filtered for subagent use.
    pub(crate) fn internal_tool_definitions_for_subagent(
        &self,
        allowed_tools: Option<&[String]>,
    ) -> Vec<hydeclaw_types::ToolDefinition> {
        crate::agent::pipeline::tool_defs::filter_for_subagent(
            self.internal_tool_definitions(),
            crate::agent::pipeline::subagent::SUBAGENT_DENIED_TOOLS,
            allowed_tools,
        )
    }

    // ── Parallel tool dispatch (from engine_parallel.rs) ─────────────────────

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_tool_calls_partitioned(
        &self,
        tool_calls: &[hydeclaw_types::ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut LoopDetector,
        detect_loops: bool,
        persist_ctx: Option<&crate::agent::pipeline::parallel::ToolPersistCtx<'_>>,
    ) -> Result<Vec<crate::agent::pipeline::parallel::ToolBatchResult>, LoopBreak> {
        // Load YAML tools (cached for 30s)
        let yaml_tools: std::sync::Arc<std::collections::HashMap<String, crate::tools::yaml_tools::YamlToolDef>> = {
            let cache = self.tex().yaml_tools_cache.read().await;
            if cache.0.elapsed() < std::time::Duration::from_secs(30) && !cache.1.is_empty() {
                std::sync::Arc::clone(&cache.1)
            } else {
                drop(cache);
                let tools = std::sync::Arc::new(
                    crate::tools::yaml_tools::load_yaml_tools(&self.cfg().workspace_dir, false)
                        .await
                        .into_iter()
                        .map(|t| (t.name.clone(), t))
                        .collect::<std::collections::HashMap<String, crate::tools::yaml_tools::YamlToolDef>>(),
                );
                *self.tex().yaml_tools_cache.write().await =
                    (std::time::Instant::now(), std::sync::Arc::clone(&tools));
                tools
            }
        };

        crate::agent::pipeline::parallel::execute_tool_calls_partitioned(
            tool_calls,
            context,
            session_id,
            channel,
            &self.cfg().agent.model,
            current_context_chars,
            detector,
            detect_loops,
            &self.cfg().db,
            &self.cfg().embedder,
            &yaml_tools,
            self,
            persist_ctx,
        )
        .await
    }
}

/// All system (internal) tool names — single source of truth.
pub fn all_system_tool_names() -> &'static [&'static str] {
    crate::agent::pipeline::tool_defs::all_system_tool_names()
}

// ── ToolExecutorDeps impl ─────────────────────────────────────────────────────

#[async_trait::async_trait]
impl crate::agent::tool_executor::ToolExecutorDeps for AgentEngine {
    async fn execute_tool_calls_partitioned_raw(
        &self,
        tool_calls: &[hydeclaw_types::ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut crate::agent::tool_loop::LoopDetector,
        detect_loops: bool,
        persist_ctx: Option<&crate::agent::pipeline::parallel::ToolPersistCtx<'_>>,
    ) -> Result<Vec<crate::agent::pipeline::parallel::ToolBatchResult>, LoopBreak> {
        self.execute_tool_calls_partitioned(
            tool_calls,
            context,
            session_id,
            channel,
            current_context_chars,
            detector,
            detect_loops,
            persist_ctx,
        )
        .await
    }
}

// ── pipeline::parallel::ToolExecutor impl (from engine_parallel.rs) ──────────

impl crate::agent::pipeline::parallel::ToolExecutor for AgentEngine {
    fn execute_tool_call<'a>(
        &'a self,
        name: &'a str,
        arguments: &'a serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>> {
        self.execute_tool_call(name, arguments)
    }

    fn needs_approval(&self, tool_name: &str) -> bool {
        self.needs_approval(tool_name)
    }

    /// Reads `AppConfig.agent_tool.safety_timeout_secs` at call time so config
    /// hot-reload takes effect on the next tool batch.
    fn agent_safety_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.cfg().app_config.agent_tool.safety_timeout_secs,
        )
    }
}

// ── pipeline::llm_call::Compactor impl (from engine_provider.rs) ─────────────

/// `AgentEngine` acts as its own compactor — delegates to `compact_messages`.
#[async_trait::async_trait]
impl crate::agent::pipeline::llm_call::Compactor for AgentEngine {
    async fn compact(&self, messages: &mut Vec<Message>) {
        self.compact_messages(messages, None).await;
    }
}
