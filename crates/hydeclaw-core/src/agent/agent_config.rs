// ── AgentConfig — immutable snapshot of agent configuration ─────────────────

use std::sync::Arc;

use sqlx::PgPool;

use crate::agent::approval_manager::ApprovalManager;
use crate::agent::memory_service::MemoryService;
use crate::agent::providers::LlmProvider;
use crate::agent::session_agent_pool::SessionPoolsMap;
use crate::config::{AgentSettings, AppConfig};
use crate::db::audit_queue::AuditQueue;
use crate::gateway::state::AgentMap;
use crate::memory::EmbeddingService;
use crate::scheduler::Scheduler;
use crate::tools::ToolRegistry;

/// Immutable snapshot of everything an agent needs to operate.
///
/// Grouped into five concern areas: identity, LLM, data, tools, and infra.
/// All fields are either `Clone`-cheap (`Arc`, `PgPool`) or small value types.
///
/// All engine code reads from this struct via `engine.cfg()`.
pub struct AgentConfig {
    // ── Identity ────────────────────────────────────────────────────────
    pub agent: AgentSettings,
    pub workspace_dir: String,
    pub default_timezone: String,
    pub app_config: Arc<AppConfig>,

    // ── LLM ─────────────────────────────────────────────────────────────
    pub provider: Arc<dyn LlmProvider>,
    pub compaction_provider: Option<Arc<dyn LlmProvider>>,

    // ── Data ────────────────────────────────────────────────────────────
    pub db: PgPool,
    pub memory_store: Arc<dyn MemoryService>,
    pub embedder: Arc<dyn EmbeddingService>,

    // ── Tools ───────────────────────────────────────────────────────────
    pub tools: ToolRegistry,
    pub approval_manager: Arc<ApprovalManager>,

    // ── Infra ───────────────────────────────────────────────────────────
    pub scheduler: Option<Arc<Scheduler>>,
    pub agent_map: Option<AgentMap>,
    pub session_pools: Option<SessionPoolsMap>,
    /// Per-session tool dispatcher state map (describe cache, call counts,
    /// promoted system extensions). `None` for engines created outside of
    /// `AgentCore` (e.g. some test helpers).
    pub session_tool_state: Option<crate::agent::dispatcher::SessionToolStateMap>,
    pub audit_queue: Arc<AuditQueue>,
    /// Phase 65 OBS-02: process-wide metrics registry for recording tool
    /// latency, LLM call duration, and token usage. Cloned from
    /// `InfraServices.metrics` at engine construction time.
    pub metrics: Arc<crate::metrics::MetricsRegistry>,
    /// Shared YAML-tool response cache (process-wide singleton). Cloned
    /// from `AgentDeps.tool_exec_ctx` at engine construction time.
    /// Read by engine_dispatch in Task 8 — kept allocated already so Task 8
    /// is a pure wiring patch with no struct churn.
    #[allow(dead_code)]
    pub tool_exec_ctx: Arc<crate::tools::yaml_tools::ToolExecutionContext>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion: `AgentConfig` must be `Send + Sync` so it can
    /// live inside `Arc` and be shared across tokio tasks.
    #[test]
    fn agent_config_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentConfig>();
    }
}
