use anyhow::Result;
use opex_types::{Message, MessageRole, ToolDefinition};
use sqlx::PgPool;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

use super::channel_actions::ChannelActionRouter;
use super::providers::LlmProvider;
use crate::mcp::McpRegistry;


pub(crate) use crate::agent::pipeline::subagent::parse_subagent_timeout;
pub mod run;

// ── Sub-modules ──────────────────────────────────────────────────────────────
pub mod stream;
pub mod approval_flow;
pub mod yaml_tool_runner;
pub mod context_builder;
pub mod tool_executor;
pub mod loop_detector_integration;

// Re-exports so external call-sites keep resolving without change.
pub use self::stream::{ProcessingPhase, StreamEvent};
pub use self::approval_flow::ApprovalResult;
pub(crate) use self::yaml_tool_runner::SecretsEnvResolver;
// engine_dispatch.rs is #[path]-included and reaches this via `use super::*;`.
pub(super) use self::yaml_tool_runner::CACHEABLE_SEARCH_TOOLS;
pub use self::tool_executor::all_system_tool_names;

/// A background process started by the `process_start` tool (base agents only).
#[allow(dead_code)] // process_id/command/started_at are diagnostic metadata
                    // surfaced via tool result strings, not Rust field reads.
pub struct BgProcess {
    pub process_id: String,
    pub command: String,
    pub log_path: String,
    pub pid: Option<u32>,
    pub started_at: std::time::Instant,
}

pub struct AgentEngine {
    /// Context builder — builds session/messages/tools for each LLM call.
    /// Initialized via `set_context_builder` after engine Arc creation.
    /// Holds `Arc<dyn ContextBuilder>` for testability.
    pub context_builder: OnceLock<Arc<dyn crate::agent::context_builder::ContextBuilder>>,
    /// Tool executor — owns tool-only state (sandbox, caches, subagent registry, etc.).
    /// Stored as concrete `Arc<DefaultToolExecutor>` for direct field access in engine methods.
    /// Initialized via `set_tool_executor` after engine Arc creation.
    pub tool_executor: OnceLock<Arc<crate::agent::tool_executor::DefaultToolExecutor>>,
    /// Per-agent mutable state (cancel/drain for shutdown, runtime fields).
    pub state: Arc<crate::agent::agent_state::AgentState>,
    /// Immutable agent configuration snapshot — sole source for agent settings,
    /// DB pool, provider, tools, memory, etc.
    pub cfg: Option<Arc<crate::agent::agent_config::AgentConfig>>,
    /// System tool registry.
    pub(crate) tool_registry: std::sync::Arc<crate::agent::tool_registry::SystemToolRegistry>,
}

/// Snapshot of what's currently displayed on the canvas.
#[derive(Debug, Clone)]
pub struct CanvasContent {
    pub content_type: String,
    pub content: String,
    pub title: Option<String>,
}


/// Maximum canvas content size (5 MB) to protect constrained environments.
pub(crate) const CANVAS_MAX_BYTES: usize = 5 * 1024 * 1024;

/// In-band marker prefix for rich card tool results.
pub(crate) const RICH_CARD_PREFIX: &str = "__rich_card__:";

/// In-band marker prefix for file/media tool results (image, audio, etc.).
/// Format: `__file__:{"url":"...","mediaType":"image/png"}`
pub(crate) const FILE_PREFIX: &str = "__file__:";

/// In-band marker prefix for nested tool-call events from codemode
/// (`code_orchestrate`). The SDK in the sandbox prints these lines to stdout
/// when dispatching tool calls through the loopback endpoint; the pipeline
/// parses them and emits `StreamEvent::ToolCallStart` / `ToolResult` so the
/// UI/timeline shows nested tool calls as a subtree.
/// Format: `__tool_call__:{"index":0,"tool":"workspace_read","status":"start","input":{"path":"foo"}}`
pub(crate) const TOOL_CALL_PREFIX: &str = "__tool_call__:";


use crate::agent::session_manager::SessionManager;

/// Convert a DB `MessageRow` into a typed Message.
/// Parses `tool_calls` JSON exactly once per row (ENG-02).
pub(crate) fn row_to_message(row: &crate::db::sessions::MessageRow) -> Message {
    let tool_calls = row.tool_calls.as_ref().and_then(|tc| {
        serde_json::from_value::<Vec<opex_types::ToolCall>>(tc.clone()).ok()
    });
    let thinking_blocks = row.thinking_blocks.as_ref()
        .and_then(|tb| serde_json::from_value::<Vec<opex_types::ThinkingBlock>>(tb.clone()).ok())
        .unwrap_or_default();
    Message {
        role: match row.role.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            _ => MessageRole::User,
        },
        content: row.content.clone(),
        tool_calls,
        tool_call_id: row
            .tool_call_id
            .clone()
            .map(opex_types::ids::ToolCallId::from),
        thinking_blocks,
        db_id: Some(opex_types::ids::MessageId::from(row.id)),
    }
}

impl AgentEngine {
    // ── Public accessors (sealed API) ──────────────────────────────

    /// Access the immutable config snapshot.
    /// Panics if called on an engine that was not constructed with a config
    /// (should not happen for top-level engines).
    pub fn cfg(&self) -> &crate::agent::agent_config::AgentConfig {
        self.cfg
            .as_ref()
            .expect("cfg not set — engine was not constructed with AgentConfig")
    }

    /// Access the mutable per-agent state (cancel/drain, runtime fields).
    pub fn state(&self) -> &crate::agent::agent_state::AgentState {
        &self.state
    }

    /// Agent name (from config).
    pub fn name(&self) -> &str {
        &self.cfg().agent.name
    }

    /// Primary model name (from config).
    pub fn model_name(&self) -> String {
        self.cfg().agent.model.clone()
    }

    /// Borrow the database pool.
    pub fn db_pool(&self) -> &PgPool {
        &self.cfg().db
    }

    /// Clone the LLM provider Arc for use outside the engine.
    pub fn provider_arc(&self) -> Arc<dyn LlmProvider> {
        self.cfg().provider.clone()
    }

    /// Read the current channel formatting prompt.
    pub async fn formatting_prompt(&self) -> Option<String> {
        self.state().channel_formatting_prompt.read().await.clone()
    }

    /// Borrow the channel action router, if configured.
    pub fn channel_router_ref(&self) -> Option<&ChannelActionRouter> {
        self.state().channel_router.as_ref()
    }

    /// Borrow the agent access config, if set.
    pub fn agent_access(&self) -> Option<&crate::config::AgentAccessConfig> {
        self.cfg().agent.access.as_ref()
    }

    /// Delegate model override to the underlying provider.
    pub fn set_model_override(&self, model: Option<String>) {
        self.cfg().provider.set_model_override(model);
    }

    /// Return the current active model name from the provider.
    pub fn current_model(&self) -> String {
        self.cfg().provider.current_model()
    }

    // ── Lifecycle ──────────────────────────────────────────────────

    /// Initialize the context builder after engine Arc creation.
    /// Must be called once after engine Arc creation.
    /// Uses `Weak<dyn ContextBuilderDeps>` to break Arc reference cycle.
    pub fn set_context_builder(&self, arc: &Arc<AgentEngine>) {
        use crate::agent::context_builder::{ContextBuilderDeps, DefaultContextBuilder};
        let deps_strong = arc.clone() as Arc<dyn ContextBuilderDeps>;
        let deps_weak = Arc::downgrade(&deps_strong);
        let builder = Arc::new(DefaultContextBuilder::new(deps_weak))
            as Arc<dyn crate::agent::context_builder::ContextBuilder>;
        let _ = self.context_builder.set(builder);
    }

    /// Initialize the tool executor after engine Arc creation.
    /// Accepts a pre-built Arc<DefaultToolExecutor> constructed in agents.rs with migrated fields.
    pub fn set_tool_executor(&self, executor: Arc<crate::agent::tool_executor::DefaultToolExecutor>) {
        use crate::agent::tool_executor::ToolExecutor;
        let executor_trait: Arc<dyn ToolExecutor> = executor.clone();
        executor.set_self_ref(&executor_trait);
        let _ = self.tool_executor.set(executor);
    }

    // ── Proxy accessors into DefaultToolExecutor ──────────────────────────────
    // Engine sub-modules and providers use these instead of direct struct field access.

    #[inline]
    pub(crate) fn tex(&self) -> &crate::agent::tool_executor::DefaultToolExecutor {
        self.tool_executor.get().expect("tool_executor not initialized")
    }

    /// Sandbox accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn sandbox(&self) -> &Option<Arc<crate::containers::sandbox::CodeSandbox>> {
        &self.tex().sandbox
    }

    /// SSRF-safe HTTP client accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn ssrf_http_client(&self) -> &reqwest::Client {
        &self.tex().ssrf_http_client
    }

    /// Tool embed cache accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn tool_embed_cache(&self) -> &Arc<crate::tools::embedding::ToolEmbeddingCache> {
        &self.tex().tool_embed_cache
    }

    /// Subagent registry accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn subagent_registry(&self) -> &crate::agent::subagent_state::SubagentRegistry {
        &self.tex().subagent_registry
    }

    /// OAuth manager accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn oauth(&self) -> &Option<Arc<crate::oauth::OAuthManager>> {
        &self.tex().oauth
    }

    /// Secrets vault accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn secrets(&self) -> &Arc<crate::secrets::SecretsManager> {
        &self.tex().secrets
    }

    /// MCP registry accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn mcp(&self) -> &Option<Arc<McpRegistry>> {
        &self.tex().mcp
    }

    /// Standard HTTP client accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.tex().http_client
    }

    /// Hooks registry accessor — delegates to `DefaultToolExecutor`.
    #[inline]
    pub(crate) fn hooks(&self) -> &Arc<super::hooks::HookRegistry> {
        &self.tex().hooks
    }

    /// SSE event TX accessor — delegates to `DefaultToolExecutor`.
    ///
    /// Phase 62 RES-01: stores an `EngineEventSender` (bounded-channel wrapper
    /// that enforces text-delta-droppable / non-text-never-dropped contract).
    #[inline]
    pub(crate) fn sse_event_tx(&self) -> &Arc<dashmap::DashMap<uuid::Uuid, crate::agent::engine_event_sender::EngineEventSender>> {
        &self.tex().sse_event_tx
    }

    // ── Codemode (tools-as-code) helpers ──────────────────────────────────────
    //
    // Used by the /api/sandbox/tool-call + /api/sandbox/tool-search loopback
    // endpoints (gateway/handlers/sandbox.rs) to resolve an agent's visible
    // tool set and dispatch tool calls from sandbox scripts.

    /// Return the agent's available system tool names (filtered by the agent's
    /// tool policy). Used by the codemode endpoint to build the allow-list for
    /// the capability token + enforce per-call policy.
    pub(crate) fn codemode_available_tool_names(&self) -> std::collections::HashSet<String> {
        let cfg = self.cfg();
        let groups = crate::agent::pipeline::tool_defs::resolve_tool_groups(cfg.agent.tools.as_ref());
        let browser_url = crate::agent::pipeline::canvas::browser_renderer_url();
        let ctx = crate::agent::pipeline::tool_defs::ToolDefsContext {
            is_base: cfg.agent.base,
            groups,
            default_timezone: &cfg.default_timezone,
            has_sandbox: self.sandbox().is_some(),
            browser_renderer_url: &browser_url,
        };
        let defs = crate::agent::pipeline::tool_defs::build_internal_tool_definitions(&ctx);
        crate::agent::pipeline::dispatch::filter_tools_by_policy(
            defs,
            cfg.agent.tools.as_ref(),
            cfg.memory_store.is_available(),
        )
        .into_iter()
        .map(|d| d.name)
        .collect()
    }

    /// Return the agent's full tool definitions (system + YAML + MCP) for
    /// codemode `tools.search`. These are the same definitions the LLM sees.
    pub(crate) fn tool_definitions_for_search(&self) -> Vec<opex_types::ToolDefinition> {
        let cfg = self.cfg();
        let groups = crate::agent::pipeline::tool_defs::resolve_tool_groups(cfg.agent.tools.as_ref());
        let browser_url = crate::agent::pipeline::canvas::browser_renderer_url();
        let ctx = crate::agent::pipeline::tool_defs::ToolDefsContext {
            is_base: cfg.agent.base,
            groups,
            default_timezone: &cfg.default_timezone,
            has_sandbox: self.sandbox().is_some(),
            browser_renderer_url: &browser_url,
        };
        let defs = crate::agent::pipeline::tool_defs::build_internal_tool_definitions(&ctx);
        crate::agent::pipeline::dispatch::filter_tools_by_policy(
            defs,
            cfg.agent.tools.as_ref(),
            cfg.memory_store.is_available(),
        )
    }

    /// Check if an enabled tool exists: capability-tools check active provider,
    /// YAML tools check workspace/tools/ file presence + status.
    async fn has_tool(&self, name: &str) -> bool {
        // Capability-инструменты: доступны, если есть активный провайдер.
        if crate::agent::capability_tools::is_capability_tool(name) {
            return crate::agent::capability_tools::find_capability_tool(&self.cfg().db, name)
                .await
                .is_some();
        }
        let dir = std::path::Path::new(&self.cfg().workspace_dir).join("tools");
        let path = dir.join(format!("{name}.yaml"));
        let path = match tokio::fs::try_exists(&path).await {
            Ok(true) => path,
            Ok(false) => {
                let yml = dir.join(format!("{name}.yml"));
                match tokio::fs::try_exists(&yml).await {
                    Ok(true) => yml,
                    Ok(false) => return false,
                    Err(e) => {
                        tracing::warn!(path = %yml.display(), error = %e, "failed to check tool file existence");
                        return false;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to check tool file existence");
                return false;
            }
        };
        // Disabled tools should not count as available
        match tokio::fs::read_to_string(&path).await {
            Ok(c) => !c.contains("\nstatus: disabled"),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read tool file for status check");
                false
            }
        }
    }

    /// Trim session messages if `max_messages` is configured.
    pub(super) async fn maybe_trim_session(&self, session_id: Uuid) {
        if let Some(max) = self.cfg().agent.session.as_ref().and_then(|s| {
            if s.max_messages > 0 { Some(s.max_messages) } else { None }
        }) {
            let sm = SessionManager::new(self.cfg().db.clone());
            if let Err(e) = sm.trim_messages(session_id, max).await {
                tracing::warn!(error = %e, "failed to trim session messages");
            }
        }
    }

    // handle() + handle_isolated() — moved to self::stream (REF-01 task 2).
    //
    // runtime_context / get_channel_info / invalidate_channel_cache /
    // load_channel_info_from_db / build_memory_context / index_facts_to_memory /
    // build_context / compact_tool_results / compaction_params / compact_messages /
    // compact_session / handle_command — moved to self::context_builder (REF-01 task 5).

    // tool_groups / internal_tool_definitions / internal_tool_definitions_for_subagent
    // -- moved to self::tool_executor (REF-01 task 6).
}

// tool_groups / internal_tool_definitions / internal_tool_definitions_for_subagent /
// execute_tool_calls_partitioned / all_system_tool_names() / ToolExecutorDeps /
// parallel::ToolExecutor / llm_call::Compactor -- moved to self::tool_executor
// (REF-01 task 6).

// Legacy `#[path]` bridge: engine_dispatch.rs holds the dispatch inherent
// methods (execute_tool_call, record_usage, apply_tool_policy_override, etc.).
// Kept here so they continue to resolve via `use super::*;` inside the leaf.
#[path = "../engine_dispatch.rs"]
mod dispatch_impl;

// tool_loop_config / create_fallback_provider / check_budget / chat_* /
// audit / handle_openai -- moved to self::loop_detector_integration
// (REF-01 task 7).


// ── Thin wrappers delegating to pipeline free functions (Phase 2) ─────────────

impl AgentEngine {
    pub async fn send_channel_message(&self, channel: &str, chat_id: i64, text: &str) -> anyhow::Result<()> {
        let ctx = crate::agent::pipeline::CommandContext { cfg: self.cfg(), state: self.state(), tex: self.tex(), subagent_depth: 0 };
        crate::agent::pipeline::channel_actions::send_channel_message(&ctx, channel, chat_id, text).await
    }

    pub(super) async fn execute_yaml_channel_action(
        &self,
        tool: &crate::tools::yaml_tools::YamlToolDef,
        args: &serde_json::Value,
        ca: &crate::tools::yaml_tools::ChannelActionConfig,
    ) -> String {
        let ctx = crate::agent::pipeline::CommandContext { cfg: self.cfg(), state: self.state(), tex: self.tex(), subagent_depth: 0 };
        crate::agent::pipeline::channel_actions::execute_yaml_channel_action(&ctx, tool, args, ca).await
    }
}

// ── Thin wrappers delegating to pipeline::subagent_runner (Phase 2) ───────────

impl AgentEngine {
    pub async fn run_subagent(
        &self,
        task: &str,
        max_iterations: usize,
        deadline: Option<std::time::Instant>,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        handle: Option<Arc<tokio::sync::RwLock<crate::agent::subagent_state::SubagentHandle>>>,
        allowed_tools: Option<Vec<String>>,
    ) -> Result<String> {
        self.run_subagent_with_session(task, max_iterations, deadline, cancel, handle, allowed_tools, None, 0).await
    }

    /// `depth` is the subagent recursion depth this run is operating at.
    /// Top-level entry points (gateway dispatch, cron) pass `0`. The session
    /// agent pool's processing loop forwards the depth that was assigned to
    /// the live agent at spawn time, so nested `agent` tool calls can be
    /// gated by `[agent.delegation] max_depth`.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_subagent_with_session(
        &self,
        task: &str,
        max_iterations: usize,
        deadline: Option<std::time::Instant>,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        handle: Option<Arc<tokio::sync::RwLock<crate::agent::subagent_state::SubagentHandle>>>,
        allowed_tools: Option<Vec<String>>,
        session_id: Option<uuid::Uuid>,
        depth: u8,
    ) -> Result<String> {
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: self.cfg(),
            state: self.state(),
            tex: self.tex(),
            subagent_depth: depth,
        };
        crate::agent::pipeline::subagent_runner::run_subagent_with_session(
            &ctx, self, task, max_iterations, deadline, cancel, handle, allowed_tools, session_id, depth,
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // search_cache_key / CACHEABLE_SEARCH_TOOLS tests — moved to
    // self::yaml_tool_runner (REF-01 task 4).

    #[test]
    fn agent_in_system_tool_names() {
        let names = all_system_tool_names();
        assert!(names.contains(&"agent"), "agent must be in all_system_tool_names()");
    }
}

