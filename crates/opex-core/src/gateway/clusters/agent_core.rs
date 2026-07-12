#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::engine::AgentEngine;
use crate::agent::session_agent_pool::SessionPoolsMap;
use crate::gateway::state::{AgentDeps, AgentMap};
use crate::scheduler::Scheduler;
use crate::tools::ToolRegistry;

// ── AgentCore cluster ─────────────────────────────────────────────────────────

/// Cluster holding all agent-lifecycle state:
/// the running agent map, shared creation deps, per-session subagent pools,
/// the tool registry, and the cron scheduler.
#[derive(Clone)]
pub struct AgentCore {
    /// Running agent handles (mutable: agents can be added/removed at runtime).
    pub map: AgentMap,
    /// Shared deps for starting new agents at runtime (RwLock for hot-update via PUT /api/config).
    pub deps: Arc<tokio::sync::RwLock<AgentDeps>>,
    /// Session-scoped agent pools: maps session UUID → pool of alive agents.
    pub session_pools: SessionPoolsMap,
    /// Per-session tool dispatcher state (describe cache, call counts, promotions).
    pub session_tool_state: crate::agent::dispatcher::SessionToolStateMap,
    /// Registered tool definitions (YAML + service tools).
    pub tools: ToolRegistry,
    /// Cron scheduler for heartbeat and dynamic jobs.
    pub scheduler: Arc<Scheduler>,
}

impl AgentCore {
    pub fn new(
        map: AgentMap,
        deps: Arc<tokio::sync::RwLock<AgentDeps>>,
        session_pools: SessionPoolsMap,
        session_tool_state: crate::agent::dispatcher::SessionToolStateMap,
        tools: ToolRegistry,
        scheduler: Arc<Scheduler>,
    ) -> Self {
        Self { map, deps, session_pools, session_tool_state, tools, scheduler }
    }

    // ── Agent lookup helpers ─────────────────────────────────────────────────

    /// Get an engine by agent name (read-locks the agents map briefly).
    pub async fn get_engine(&self, name: &str) -> Option<Arc<AgentEngine>> {
        self.map.read().await.get(name).map(|h| h.engine.clone())
    }

    /// Get a snapshot map of all running agent engines.
    pub async fn get_engines_map(&self) -> HashMap<String, Arc<AgentEngine>> {
        self.map.read().await.iter()
            .map(|(k, v)| (k.clone(), v.engine.clone()))
            .collect()
    }

    /// Get the first available engine (arbitrary order).
    pub async fn first_engine(&self) -> Option<Arc<AgentEngine>> {
        self.map.read().await.values().next().map(|h| h.engine.clone())
    }

    /// Первый агент с `base = true` (респондер self-healing). None если base-агентов нет.
    pub async fn base_engine(&self) -> Option<Arc<AgentEngine>> {
        self.map
            .read()
            .await
            .values()
            .find(|h| h.engine.cfg().agent.base)
            .map(|h| h.engine.clone())
    }

    /// Phase 65 OBS-05: total pending approval waiters across every running
    /// agent. Each agent owns its own waiters map (keyed by approval UUID);
    /// we aggregate them here for the `/api/health/dashboard` snapshot so
    /// operators can see at a glance whether approvals are backing up.
    ///
    /// Read-locks the agent map briefly, then reads each per-executor
    /// DashMap `.len()` (sync, sharded). Expected n ≤ 20 on Pi — negligible cost.
    pub async fn approval_waiters_size(&self) -> u64 {
        let map = self.map.read().await;
        let mut total: u64 = 0;
        for handle in map.values() {
            let tex = handle.engine.tex();
            // DashMap::len() is sync — no `.await` needed.
            total += tex.approval_waiters.len() as u64;
        }
        total
    }

    /// Get list of running agent names (base agents first, then alphabetical).
    pub async fn agent_names(&self) -> Vec<String> {
        let mut names: Vec<(bool, String)> = self.map.read().await.values()
            .map(|h| (h.engine.cfg().agent.base, h.engine.cfg().agent.name.clone()))
            .collect();
        names.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        names.into_iter().map(|(_, n)| n).collect()
    }

    /// Get list of running agents with name (base agents first, then alphabetical).
    pub async fn agent_summaries(&self) -> Vec<serde_json::Value> {
        let mut summaries: Vec<(bool, String, serde_json::Value)> =
            self.map.read().await.values()
                .map(|h| {
                    (
                        h.engine.cfg().agent.base,
                        h.engine.cfg().agent.name.clone(),
                        serde_json::json!({
                            "name": h.engine.cfg().agent.name,
                        }),
                    )
                })
                .collect();
        summaries.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        summaries.into_iter().map(|(_, _, v)| v).collect()
    }

    // ── Test helpers ─────────────────────────────────────────────────────────

    /// Construct a minimal `AgentCore` for unit tests that require a sync constructor.
    /// Must be called from within a tokio runtime context (e.g. inside `#[tokio::test]`).
    /// Prefer `test_empty().await` when the async form is sufficient.
    ///
    /// `Scheduler::new_noop()` is async; we drive it synchronously via
    /// `block_in_place` so the caller's signature stays `fn` rather than `async fn`.
    #[cfg(test)]
    pub fn test_new() -> Self {
        let scheduler = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(crate::scheduler::Scheduler::new_noop())
        });
        Self {
            map: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            deps: Arc::new(tokio::sync::RwLock::new(AgentDeps::test_new())),
            session_pools: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            session_tool_state: Arc::new(dashmap::DashMap::new()),
            tools: ToolRegistry::empty(),
            scheduler,
        }
    }

    /// Construct a minimal `AgentCore` for unit tests (no DB, no scheduler running).
    #[cfg(test)]
    pub async fn test_empty() -> Self {
        use std::collections::HashMap;
        Self {
            map: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            deps: Arc::new(tokio::sync::RwLock::new(AgentDeps::test_new())),
            session_pools: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            session_tool_state: Arc::new(dashmap::DashMap::new()),
            tools: ToolRegistry::empty(),
            scheduler: Scheduler::new_noop().await,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn agent_core_empty_has_no_agents() {
        let core = AgentCore::test_empty().await;
        assert_eq!(core.agent_names().await.len(), 0);
    }

    #[tokio::test]
    async fn agent_core_get_engine_returns_none_when_empty() {
        let core = AgentCore::test_empty().await;
        assert!(core.get_engine("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn agent_core_first_engine_returns_none_when_empty() {
        let core = AgentCore::test_empty().await;
        assert!(core.first_engine().await.is_none());
    }

    #[tokio::test]
    async fn agent_core_get_engines_map_returns_empty_map() {
        let core = AgentCore::test_empty().await;
        assert!(core.get_engines_map().await.is_empty());
    }

    #[tokio::test]
    async fn agent_core_agent_summaries_returns_empty_when_no_agents() {
        let core = AgentCore::test_empty().await;
        assert!(core.agent_summaries().await.is_empty());
    }

    #[tokio::test]
    async fn agent_core_clone_shares_map_arc() {
        let core = AgentCore::test_empty().await;
        let core2 = core.clone();
        assert!(Arc::ptr_eq(&core.map, &core2.map));
    }

    // block_in_place requires the multi-threaded runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_core_test_new_is_sync() {
        // Verify the sync test_new() exists and produces an empty core.
        // (Separate from test_empty() which is async.)
        let _core = AgentCore::test_new();
    }
}
