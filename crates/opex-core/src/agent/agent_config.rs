// ── AgentConfig — immutable snapshot of agent configuration ─────────────────

use std::sync::Arc;

use sqlx::PgPool;

use crate::agent::approval_manager::ApprovalManager;
use crate::agent::clarify_manager::ClarifyManager;
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
    /// Resolved capability slots from the agent's profile (`profiles` table).
    /// Populated once at engine construction via
    /// `profile_resolver::resolve_slots_for_agent`; the primary (`text`) and
    /// `compaction` providers above are derived from it. Available to all
    /// downstream code as `engine.cfg().profile_slots`. The `text` chain's
    /// reserve entries (`text[1..]`) drive LLM failover via
    /// `create_fallback_provider`; other capability slots
    /// (stt/tts/vision/imagegen/websearch) are read by their own subsystems.
    pub profile_slots: crate::db::profiles::Slots,

    // ── Data ────────────────────────────────────────────────────────────
    pub db: PgPool,
    pub memory_store: Arc<dyn MemoryService>,
    pub embedder: Arc<dyn EmbeddingService>,
    /// Shared discovery cache of toolgate-hosted file handlers (process-wide
    /// singleton, cloned from `AppState.handlers`). Cheap to clone — inner
    /// `Arc<RwLock<HandlerCache>>` is shared, so command dispatch (`dispatch.rs`)
    /// and `/help` reuse the same ETag cache instead of paying a full manifest
    /// fetch on every `/`-prefixed message.
    pub handler_registry: crate::agent::handler_registry::HandlerRegistry,

    // ── Tools ───────────────────────────────────────────────────────────
    pub tools: ToolRegistry,
    pub approval_manager: Arc<ApprovalManager>,
    pub clarify_manager: Arc<ClarifyManager>,

    // ── Infra ───────────────────────────────────────────────────────────
    pub scheduler: Option<Arc<Scheduler>>,
    pub agent_map: Option<AgentMap>,
    pub session_pools: Option<SessionPoolsMap>,
    /// Per-session `/goal` autonomous-loop driver registry + the goal/user
    /// serialization lock. `None` for engines created outside `AgentCore`.
    pub goal_pool: Option<crate::agent::goal::pool::GoalDriverPool>,
    pub goal_locks: Option<crate::agent::goal::pool::GoalLocks>,
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
    /// from `AgentDeps.tool_exec_ctx` at engine construction time. Read by
    /// `engine_dispatch::execute_tool_call_inner` on the YAML-tool path.
    pub tool_exec_ctx: Arc<crate::tools::yaml_tools::ToolExecutionContext>,
    /// Shared checkpoint manager (process-wide singleton). `None` for engines
    /// created outside `AgentCore` (test helpers). Cloned from `AgentDeps.checkpoint_mgr`.
    pub checkpoint_manager: Option<Arc<crate::agent::checkpoint_manager::CheckpointManager>>,
    /// Per-agent soul reflection runtime state (reflection lock + failure
    /// backoff). One instance per agent, injected into `SoulDeps` at finalize
    /// time so the reflection engine's lock/backoff is NOT a global static
    /// (spec §3/§9). `Arc::default()` at construction.
    pub soul_runtime: Arc<crate::agent::soul::reflection::SoulRuntime>,
    /// Per-session persona-drift cache (spec stage B/v2 §3): session_id →
    /// `CachedDrift` — the frozen baseline `centroid`/`mu`/`sigma` (established
    /// once per session, reused each turn) PLUS the mutable Schmitt-hysteresis
    /// `anchor_active` state (read-modify-written under a single `get_mut` per
    /// probe, spec §4.2). Process-local (survives across turns, resets on agent
    /// hot-reload — fail-soft re-establish; a rebuild also wipes `anchor_active`
    /// for every in-flight session, accepted during the `correct=false` canary
    /// window per spec §4.5). `Arc::default()` at construction. Soft-capped in
    /// the drift_probe writer to bound memory.
    pub drift_baselines: std::sync::Arc<dashmap::DashMap<uuid::Uuid, crate::agent::drift::CachedDrift>>,
    /// Shared LSP manager (process-wide singleton). `None` when LSP is disabled or
    /// for engines created outside `AgentCore` (test helpers).
    /// Cloned from `AgentDeps.lsp_manager`.
    // Task 10 wires the `lsp` tool handler that reads this field.
    #[allow(dead_code)]
    pub lsp_manager: Option<Arc<crate::agent::lsp::LspManager>>,
    /// Process-wide provider cooldown registry (Session Resilience Task 4 /
    /// WS4). Cloned from `AgentDeps.cooldowns` at engine construction time —
    /// same singleton precedent as `tool_exec_ctx` / `checkpoint_manager`, so
    /// every agent observes the same cooldown state for a shared provider
    /// name, not a per-agent copy (unlike `drift_baselines`, which is
    /// deliberately per-engine-construction/`Arc::default()`).
    pub cooldowns: Arc<crate::agent::provider_cooldown::ProviderCooldowns>,
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
