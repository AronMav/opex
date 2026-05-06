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

use hydeclaw_types::{Message, ToolDefinition};
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
    /// config via `compute_denied_tools` (so per-agent overrides /
    /// extensions take effect).
    ///
    /// `subagent_is_base` is the called subagent's own `agent.base` flag.
    /// Base subagents always receive the full tools array regardless of the
    /// parent's dispatcher settings — see `dispatch_for_subagent_decision`.
    pub(crate) fn internal_tool_definitions_for_subagent(
        &self,
        allowed_tools: Option<&[String]>,
        subagent_is_base: bool,
    ) -> Vec<hydeclaw_types::ToolDefinition> {
        let denied = crate::agent::pipeline::subagent::compute_denied_tools(
            &self.cfg().agent.delegation,
        );

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
        tool_calls: &[hydeclaw_types::ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut LoopDetector,
        detect_loops: bool,
        persist_ctx: Option<&crate::agent::pipeline::parallel::ToolPersistCtx<'_>>,
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

        // Per-session dispatcher state — pulled from the engine's
        // `session_tool_state` map by session id, when wired. None when the
        // engine was built outside of `AgentCore` (test helpers, openai_compat
        // synthetic Uuid::nil).
        let session_tool_state = self.cfg().session_tool_state.as_ref().and_then(|m| {
            m.get(&session_id).map(|r| r.value().clone())
        });
        let promotion_max = self.cfg().agent.tool_dispatcher.promotion_max;
        // Effective deny = agent.tools.deny ∪ SUBAGENT_DENIED_TOOLS (computed
        // via delegation). Without this union, a subagent could use
        // tool_use(call, name=process) to bypass the delegation block — the
        // rewrite step rejects only on `agent.tools.deny`. Spec mandates the
        // runtime deny-gate honors both. For non-subagent (parent) engines
        // the delegation list defaults to empty, so the union is harmless.
        let policy_owned = build_effective_policy(
            self.cfg().agent.tools.as_ref(),
            &self.cfg().agent.delegation,
        );
        let policy = Some(&policy_owned);

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
            policy,
            session_tool_state,
            promotion_max,
            self.mcp().as_deref(),
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

/// Build the runtime deny-effective `AgentToolPolicy` passed into
/// `pipeline::execute_tool_calls_partitioned`. The deny list is the union of
/// `agent.tools.deny` and the delegation-computed deny list (via
/// `compute_denied_tools` — `SUBAGENT_DENIED_TOOLS + blocked_tools_extra`, or
/// `blocked_tools_override` when set). All other policy fields (allow,
/// allow_all, deny_all_others, groups) come from the agent's configured
/// policy or its `Default` when unset.
///
/// Without this union, `tool_use(action="call", name=<denied>)` would slip
/// past the rewrite step's runtime deny-gate (which only checks
/// `policy.deny`). The same union is mirrored in
/// `tool_handlers/tool_use.rs::deny_list` (catalogue / describe) and
/// `engine/context_builder.rs::cfg_deny_list` (trigger-hint).
pub(crate) fn build_effective_policy(
    base: Option<&crate::config::AgentToolPolicy>,
    delegation: &crate::config::DelegationConfig,
) -> crate::config::AgentToolPolicy {
    let base = base.cloned().unwrap_or_default();
    let delegation_denied = crate::agent::pipeline::subagent::compute_denied_tools(delegation);
    let mut deny = base.deny.clone();
    for d in delegation_denied {
        if !deny.contains(&d) {
            deny.push(d);
        }
    }
    crate::config::AgentToolPolicy { deny, ..base }
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

    /// Effective policy must include both the agent's configured deny list
    /// AND the delegation-computed deny list. Closes the bypass where a
    /// subagent could use `tool_use(action="call", name=process)` to slip
    /// past the rewrite step's runtime deny-gate.
    #[test]
    fn build_effective_policy_unions_agent_deny_and_delegation_denied() {
        use crate::agent::pipeline::subagent::SUBAGENT_DENIED_TOOLS;
        use crate::config::{AgentToolPolicy, DelegationConfig};

        // Agent has its own deny entries.
        let base = AgentToolPolicy {
            deny: vec!["custom_deny".into()],
            ..Default::default()
        };
        // Default delegation → adds SUBAGENT_DENIED_TOOLS.
        let delegation = DelegationConfig::default();

        let effective = super::build_effective_policy(Some(&base), &delegation);

        // Original deny preserved.
        assert!(effective.deny.iter().any(|d| d == "custom_deny"));
        // All SUBAGENT_DENIED_TOOLS now present in the union.
        for tool in SUBAGENT_DENIED_TOOLS {
            assert!(
                effective.deny.iter().any(|d| d == *tool),
                "delegation-denied {tool} missing from effective deny — bypass risk"
            );
        }
    }

    #[test]
    fn build_effective_policy_dedupes_when_agent_already_denies_subagent_tool() {
        use crate::config::{AgentToolPolicy, DelegationConfig};

        // Agent already denies "process" (which is also a SUBAGENT_DENIED_TOOL).
        let base = AgentToolPolicy {
            deny: vec!["process".into()],
            ..Default::default()
        };
        let delegation = DelegationConfig::default();

        let effective = super::build_effective_policy(Some(&base), &delegation);

        let process_count = effective.deny.iter().filter(|d| *d == "process").count();
        assert_eq!(process_count, 1, "duplicate deny entries must be deduped");
    }

    #[test]
    fn build_effective_policy_handles_no_agent_policy() {
        use crate::config::DelegationConfig;
        use crate::agent::pipeline::subagent::SUBAGENT_DENIED_TOOLS;

        // No agent.tools section configured → falls back to Default.
        let effective = super::build_effective_policy(None, &DelegationConfig::default());

        // Effective deny still carries the delegation block.
        for tool in SUBAGENT_DENIED_TOOLS {
            assert!(effective.deny.iter().any(|d| d == *tool));
        }
    }

    #[test]
    fn build_effective_policy_with_delegation_override() {
        use crate::config::{AgentToolPolicy, DelegationConfig};

        // Delegation override replaces the default subagent deny list.
        let base = AgentToolPolicy {
            deny: vec!["agent_specific".into()],
            ..Default::default()
        };
        let delegation = DelegationConfig {
            blocked_tools_override: vec!["only_this".into()],
            ..Default::default()
        };

        let effective = super::build_effective_policy(Some(&base), &delegation);

        // Agent deny preserved, override added, default SUBAGENT_DENIED_TOOLS NOT present.
        assert!(effective.deny.iter().any(|d| d == "agent_specific"));
        assert!(effective.deny.iter().any(|d| d == "only_this"));
        assert!(!effective.deny.iter().any(|d| d == "process"),
            "blocked_tools_override should replace SUBAGENT_DENIED_TOOLS");
    }
}
