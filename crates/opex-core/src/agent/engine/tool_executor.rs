//! Trait impls that wire `AgentEngine` into the tool pipeline.
//!
//! - `impl ToolExecutorDeps for AgentEngine`
//! - `impl pipeline::parallel::ToolExecutor for AgentEngine`
//! - `impl pipeline::llm_call::Compactor for AgentEngine`
//! - inherent methods: `tool_groups`, `internal_tool_definitions[_for_subagent]`,
//!   `execute_tool_calls_partitioned`
//! - `all_system_tool_names()` (re-exported from `engine/mod.rs`)

use opex_types::{Message, ToolDefinition};
use uuid::Uuid;

use super::AgentEngine;
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
    ///
    /// The deny list is computed from the agent's `[agent.delegation]`
    /// config via `runtime_subagent_denylist` (always SUBAGENT_DENIED_TOOLS
    /// plus any `blocked_tools_extra`).
    ///
    /// `subagent_is_base` is the called subagent's own `agent.base` flag.
    /// Base subagents always receive the full tools array regardless of the
    /// parent's dispatcher settings — see `dispatch_for_subagent_decision`.
    pub(crate) fn internal_tool_definitions_for_subagent(
        &self,
        allowed_tools: Option<&[String]>,
        subagent_is_base: bool,
    ) -> Vec<opex_types::ToolDefinition> {
        // Audit 2026-05-08 (6th pass): visibility list MUST match the runtime
        // gate. Using `runtime_subagent_denylist` anchors visibility to
        // SUBAGENT_DENIED_TOOLS so denied tools are neither visible nor callable.
        let mut denied = crate::agent::pipeline::subagent::runtime_subagent_denylist(
            &self.cfg().agent.delegation,
        );
        // Mirror the base-subagent carve-out applied in `subagent_runner` so
        // base subagents see `code_exec` in their catalogue.
        if subagent_is_base {
            denied.retain(|t| t != "code_exec");
        }

        let dispatch_for_subagent = dispatch_for_subagent_decision(
            subagent_is_base,
            self.cfg().agent.tool_dispatcher.enabled,
            self.cfg().agent.delegation.subagent_dispatcher_enabled,
        );

        let mut tools = self.internal_tool_definitions();

        if dispatch_for_subagent {
            // Apply the same partition the parent uses: static core only.
            let core: std::collections::HashSet<&str> =
                crate::agent::pipeline::tool_defs::static_core_tool_names()
                    .iter()
                    .copied()
                    .collect();
            tools.retain(|t| core.contains(t.name.as_str()));
        }

        crate::agent::pipeline::tool_defs::filter_for_subagent(
            tools,
            &denied,
            allowed_tools,
        )
    }

    // ── Parallel tool dispatch (from engine_parallel.rs) ─────────────────────

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_tool_calls_partitioned(
        &self,
        tool_calls: &[opex_types::ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut LoopDetector,
        detect_loops: bool,
        persist_ctx: Option<&crate::agent::pipeline::parallel::ToolPersistCtx<'_>>,
        parallel_batch_id: Option<opex_types::ids::ParallelBatchId>,
        extra_deny: &[String],
    ) -> crate::agent::pipeline::parallel::BatchOutcome {
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

        // Runtime deny-gate uses BOTH:
        //   1. the agent's own tool_policy.deny (passed as `policy`), and
        //   2. `extra_deny`, the parent's SUBAGENT_DENIED_TOOLS when this
        //      engine is invoked as a subagent. Without (2),
        //      `tool_use(action="call", name=X)` from inside a subagent
        //      could reach a tool blocked at the delegation layer (e.g.
        //      code_exec, cron, secret_set) — closed by audit 2026-05-08.
        let policy = self.cfg().agent.tools.as_ref();

        crate::agent::pipeline::parallel::execute_tool_calls_partitioned(
            tool_calls,
            context,
            session_id,
            channel,
            // Effective model (override-aware) so truncate_tool_result's window
            // lookup matches the value resolved at bootstrap.
            &self.current_model(),
            current_context_chars,
            detector,
            detect_loops,
            &self.cfg().db,
            &self.cfg().embedder,
            &yaml_tools,
            self,
            persist_ctx,
            policy,
            extra_deny,
            self.mcp().as_deref(),
            parallel_batch_id,
        )
        .await
    }
}

/// All system (internal) tool names — single source of truth.
pub fn all_system_tool_names() -> &'static [&'static str] {
    crate::agent::pipeline::tool_defs::all_system_tool_names()
}

/// Decide whether a subagent should run with the dispatcher (compact tools
/// array) or with the full tools array. The subagent's own `is_base` flag
/// overrides everything: base subagents always receive full tools.
pub(crate) fn dispatch_for_subagent_decision(
    subagent_is_base: bool,
    parent_tool_dispatcher_enabled: bool,
    parent_subagent_override: Option<bool>,
) -> bool {
    if subagent_is_base {
        return false;
    }
    parent_subagent_override.unwrap_or(parent_tool_dispatcher_enabled)
}

// ── ToolExecutorDeps impl ─────────────────────────────────────────────────────

#[async_trait::async_trait]
impl crate::agent::tool_executor::ToolExecutorDeps for AgentEngine {
    async fn execute_tool_calls_partitioned_raw(
        &self,
        tool_calls: &[opex_types::ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut crate::agent::tool_loop::LoopDetector,
        detect_loops: bool,
        persist_ctx: Option<&crate::agent::pipeline::parallel::ToolPersistCtx<'_>>,
        parallel_batch_id: Option<opex_types::ids::ParallelBatchId>,
        extra_deny: &[String],
    ) -> crate::agent::pipeline::parallel::BatchOutcome {
        self.execute_tool_calls_partitioned(
            tool_calls,
            context,
            session_id,
            channel,
            current_context_chars,
            detector,
            detect_loops,
            persist_ctx,
            parallel_batch_id,
            extra_deny,
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

    /// Reads `AppConfig.agent_tool` (default + per-tool overrides) at call time
    /// so config hot-reload takes effect on the next tool batch.
    fn tool_timeout(&self, tool_name: &str) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.cfg().app_config.agent_tool.tool_timeout_secs(tool_name),
        )
    }

    fn semantic_cache_config(&self, tool: &str) -> Option<crate::config::SemanticCacheToolConfig> {
        self.cfg().app_config.semantic_cache.for_tool(tool)
    }
}

// ── pipeline::llm_call::Compactor impl (from engine_provider.rs) ─────────────

/// `AgentEngine` acts as its own compactor — delegates to `compact_messages`.
#[async_trait::async_trait]
impl crate::agent::pipeline::llm_call::Compactor for AgentEngine {
    async fn compact(&self, messages: &mut Vec<Message>) {
        self.compact_messages(messages, None).await;
    }

    async fn compact_force(&self, messages: &mut Vec<Message>) {
        self.compact_messages_force(messages).await;
    }
}

#[cfg(test)]
mod tests {
    use super::dispatch_for_subagent_decision as d;

    #[test]
    fn base_subagent_always_gets_full_tools() {
        // Base subagent → never dispatcher, regardless of parent flags.
        assert!(!d(true, true, None));
        assert!(!d(true, true, Some(true)));
        assert!(!d(true, false, None));
        assert!(!d(true, false, Some(true)));

        // Non-base subagent: explicit override wins, else inherit parent.
        assert!(d(false, true, None));
        assert!(!d(false, false, None));
        assert!(d(false, false, Some(true)));
        assert!(!d(false, true, Some(false)));
    }

}
