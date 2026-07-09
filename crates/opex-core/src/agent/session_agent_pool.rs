use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{mpsc, oneshot, Notify, RwLock};
use uuid::Uuid;

use super::engine::AgentEngine;

// ── Status constants ──────────────────────────────────────────────────────────

pub const STATUS_IDLE: u8 = 0;
pub const STATUS_PROCESSING: u8 = 1;

/// Type alias for the shared session pools map used across `AppState` and `AgentEngine`.
pub type SessionPoolsMap = Arc<tokio::sync::RwLock<HashMap<Uuid, SessionAgentPool>>>;

// ── AgentMessage ─────────────────────────────────────────────────────────────

/// Message sent to a `LiveAgent`'s processing loop.
pub struct AgentMessage {
    pub text: String,
    /// Per-message result channel (F061). The processing loop sends THIS
    /// message's result here when it finishes, so a waiter receives exactly the
    /// result of the message it enqueued — instead of racing every other waiter
    /// on the single shared `last_result` slot. `None` for fire-and-forget sends
    /// that don't await a result.
    pub respond_to: Option<oneshot::Sender<String>>,
}

// ── LiveAgent ─────────────────────────────────────────────────────────────────

/// An always-alive agent instance bound to a session.
pub struct LiveAgent {
    pub name: String,
    pub message_tx: mpsc::Sender<AgentMessage>,
    pub status: Arc<AtomicU8>,
    pub last_result: Arc<RwLock<Option<String>>>,
    pub cancel: Arc<AtomicBool>,
    pub created_at: Instant,
    pub iteration_count: Arc<AtomicUsize>,
    pub task_handle: tokio::task::JoinHandle<()>,
    /// Signaled (via `notify_one`) each time the agent transitions to IDLE.
    /// Callers waiting in `wait_until_idle` await this instead of polling —
    /// near-zero latency overhead. (Per-message results are delivered via each
    /// `AgentMessage::respond_to` oneshot, not this signal — F061.)
    pub result_notify: Arc<Notify>,
}

impl LiveAgent {
    /// Returns true if the agent is currently processing a message.
    pub fn is_processing(&self) -> bool {
        self.status.load(Ordering::Acquire) == STATUS_PROCESSING
    }

    /// Returns true if the agent is idle (not processing).
    pub fn is_idle(&self) -> bool {
        self.status.load(Ordering::Acquire) == STATUS_IDLE
    }

    /// Returns the number of iterations (messages processed) by this agent.
    pub fn iterations(&self) -> usize {
        self.iteration_count.load(Ordering::Relaxed)
    }

    /// Returns the elapsed time since this agent was created.
    pub fn elapsed(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }
}

impl Drop for LiveAgent {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.task_handle.abort();
    }
}

// ── SessionAgentPool ─────────────────────────────────────────────────────────

/// Hard cap on the number of session pools kept in-memory at once.
/// If the global map exceeds this limit, the oldest idle pool is evicted.
pub const SESSION_AGENT_POOL_MAX: usize = 1000;

/// Pool of always-alive agents for a single session.
pub struct SessionAgentPool {
    agents: HashMap<String, LiveAgent>,
    #[allow(dead_code)] // retained for diagnostics / future per-pool routing.
    session_id: Uuid,
    /// Tracks the last time this pool was accessed so the LRU eviction in
    /// `insert_pool_with_cap` can target the least-recently-used entry.
    pub last_activity: Instant,
}

impl SessionAgentPool {
    /// Creates a new empty pool for the given session.
    pub fn new(session_id: Uuid) -> Self {
        Self {
            agents: HashMap::new(),
            session_id,
            last_activity: Instant::now(),
        }
    }

    /// Update the last-activity timestamp (call whenever the pool is accessed).
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Returns a reference to the named agent, if present.
    pub fn get(&self, name: &str) -> Option<&LiveAgent> {
        self.agents.get(name)
    }

    /// Returns true if the pool contains an agent with the given name.
    pub fn contains(&self, name: &str) -> bool {
        self.agents.contains_key(name)
    }

    /// Inserts a live agent into the pool.
    pub fn insert(&mut self, agent: LiveAgent) {
        self.agents.insert(agent.name.clone(), agent);
    }

    /// Removes and returns the named agent from the pool, if present.
    pub fn remove(&mut self, name: &str) -> Option<LiveAgent> {
        self.agents.remove(name)
    }

    /// Returns a list of lightweight status summaries for all agents in the pool.
    pub fn list(&self) -> Vec<AgentPoolEntry> {
        self.agents
            .values()
            .map(|a| AgentPoolEntry {
                name: a.name.clone(),
                status: if a.is_processing() {
                    "processing".to_string()
                } else {
                    "idle".to_string()
                },
                iterations: a.iterations(),
                elapsed_secs: a.elapsed().as_secs_f64(),
            })
            .collect()
    }

    /// Cancels and drops all agents in the pool.
    pub fn kill_all(&mut self) {
        self.agents.clear();
    }

    /// Returns the number of agents in the pool.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Returns true if the pool contains no agents.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Returns true if all agents in the pool are idle (not processing).
    pub fn is_all_idle(&self) -> bool {
        self.agents.values().all(|a| a.is_idle())
    }

    /// Returns true if all agents in this pool have finished (`task_handle.is_finished()`).
    pub fn is_all_finished(&self) -> bool {
        self.agents.values().all(|a| a.task_handle.is_finished())
    }

    /// Returns true if all agents are idle and have been alive longer than `duration`.
    pub fn all_idle_longer_than(&self, duration: std::time::Duration) -> bool {
        !self.agents.is_empty()
            && self.agents.values().all(|a| a.is_idle() && a.elapsed() > duration)
    }
}

/// Maximum idle time for a pool before it's evicted (10 minutes).
const POOL_IDLE_TIMEOUT_SECS: u64 = 600;

/// Remove stale session pools: empty, all agents finished, or idle longer than timeout.
pub async fn cleanup_stale_pools(
    pools: &tokio::sync::RwLock<std::collections::HashMap<uuid::Uuid, SessionAgentPool>>,
) -> usize {
    let stale_ids: Vec<uuid::Uuid> = {
        let pools_read = pools.read().await;
        pools_read.iter()
            .filter(|(_, pool)| {
                pool.is_empty()
                    || pool.is_all_finished()
                    || pool.all_idle_longer_than(std::time::Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
            })
            .map(|(id, _)| *id)
            .collect()
    };
    if stale_ids.is_empty() {
        return 0;
    }
    let mut pools_write = pools.write().await;
    let mut removed = 0;
    for id in &stale_ids {
        if let Some(pool) = pools_write.get(id)
            && (pool.is_empty() || pool.is_all_finished()
                || pool.all_idle_longer_than(std::time::Duration::from_secs(POOL_IDLE_TIMEOUT_SECS)))
            {
                pools_write.remove(id);
                removed += 1;
            }
    }
    if removed > 0 {
        tracing::info!(removed, "cleaned up stale session agent pools");
    }
    removed
}

/// Insert a new `SessionAgentPool` into the global map, enforcing `SESSION_AGENT_POOL_MAX`.
///
/// Use this when you do **not** already hold the write lock. For callers that
/// already hold a write guard (e.g. `agent_tool::ask_spawn_new`) the eviction
/// logic is inlined directly to avoid a double-lock.
///
/// If the map is already at capacity, the **oldest idle** entry (smallest
/// `last_activity`, all agents idle) is evicted before insertion. If no idle
/// entry is found, the entry with the smallest `last_activity` is evicted
/// regardless of agent status (least-recently-used fallback).
///
/// Emits `tracing::warn!` when eviction occurs so operators can tune the cap.
#[allow(dead_code)] // Public API — available for callers that don't hold a write lock.
pub async fn insert_pool_with_cap(
    pools: &tokio::sync::RwLock<HashMap<Uuid, SessionAgentPool>>,
    session_id: Uuid,
    pool: SessionAgentPool,
) {
    let mut pools_write = pools.write().await;

    if pools_write.len() >= SESSION_AGENT_POOL_MAX {
        // Prefer evicting an idle pool; fall back to LRU if all are busy.
        let evict_id = pools_write
            .iter()
            .filter(|(_, p)| p.is_all_idle())
            .min_by_key(|(_, p)| p.last_activity)
            .or_else(|| {
                pools_write
                    .iter()
                    .min_by_key(|(_, p)| p.last_activity)
            })
            .map(|(id, _)| *id);

        if let Some(id) = evict_id {
            pools_write.remove(&id);
            tracing::warn!(
                evicted_session = %id,
                cap = SESSION_AGENT_POOL_MAX,
                "session agent pool cap reached — evicted oldest idle pool"
            );
        }
    }

    pools_write.insert(session_id, pool);
}

// ── AgentPoolEntry ────────────────────────────────────────────────────────────

/// Lightweight status summary for a live agent in the pool.
#[derive(Debug, Clone, Serialize)]
pub struct AgentPoolEntry {
    pub name: String,
    pub status: String,
    pub iterations: usize,
    pub elapsed_secs: f64,
}

// ── spawn_live_agent ─────────────────────────────────────────────────────────

/// Spawn a new `LiveAgent` with a background processing loop.
/// Returns `None` if the initial task could not be delivered (channel closed).
/// `session_id` is passed to `run_subagent_with_session` so pool agents can use the `agent` tool.
///
/// `depth` is the subagent recursion depth assigned to this live agent. The
/// caller (`handle_agent_ask`) computes `caller_depth + 1`. The processing
/// loop forwards `depth` into every `run_subagent_with_session` call so any
/// nested `agent` tool dispatch sees the correct depth and `check_depth_limit`
/// can gate further spawns via `[agent.delegation] max_depth`.
pub fn spawn_live_agent(
    name: String,
    engine: Arc<AgentEngine>,
    initial_task: String,
    session_id: Uuid,
    depth: u8,
) -> Option<(LiveAgent, oneshot::Receiver<String>)> {
    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let status = Arc::new(AtomicU8::new(STATUS_PROCESSING));
    let last_result = Arc::new(RwLock::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let iteration_count = Arc::new(AtomicUsize::new(0));
    let result_notify = Arc::new(Notify::new());

    let task_handle = tokio::spawn(agent_processing_loop(
        rx,
        engine.clone(),
        status.clone(),
        last_result.clone(),
        cancel.clone(),
        iteration_count.clone(),
        session_id,
        result_notify.clone(),
        depth,
    ));

    // Send initial task synchronously — channel is fresh with capacity 32.
    // The oneshot lets the spawning `ask` await THIS task's result specifically
    // (F061), even if concurrent asks enqueue more messages before it finishes.
    let (result_tx, result_rx) = oneshot::channel::<String>();
    if tx
        .try_send(AgentMessage {
            text: initial_task,
            respond_to: Some(result_tx),
        })
        .is_err()
    {
        task_handle.abort();
        return None;
    }

    Some((
        LiveAgent {
            name,
            message_tx: tx,
            status,
            last_result,
            cancel,
            created_at: Instant::now(),
            iteration_count,
            task_handle,
            result_notify,
        },
        result_rx,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn agent_processing_loop(
    mut rx: mpsc::Receiver<AgentMessage>,
    engine: Arc<AgentEngine>,
    status: Arc<AtomicU8>,
    last_result: Arc<RwLock<Option<String>>>,
    cancel: Arc<AtomicBool>,
    iteration_count: Arc<AtomicUsize>,
    session_id: Uuid,
    result_notify: Arc<Notify>,
    depth: u8,
) {
    let max_iterations = engine.tool_loop_config().effective_max_iterations();
    let timeout = crate::agent::engine::parse_subagent_timeout(
        &engine.cfg().app_config.subagents.in_process_timeout,
    );

    while let Some(msg) = rx.recv().await {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        status.store(STATUS_PROCESSING, Ordering::Relaxed);
        let deadline = Some(Instant::now() + timeout);

        // Pass session_id via run_subagent_with_session so the `agent` tool can find the
        // correct SessionAgentPool through enriched `_context`. `depth` is the depth
        // this live agent was spawned at — forwarded so nested `agent` tool calls
        // observe the parent's depth via enriched `_context.subagent_depth`.

        let result = engine
            .run_subagent_with_session(
                &msg.text,
                max_iterations,
                deadline,
                Some(cancel.clone()),
                None,
                None,
                Some(session_id),
                depth,
            )
            .await;

        let result_text = match result {
            Ok(text) => text,
            Err(e) => format!("Error: {e}"),
        };

        iteration_count.fetch_add(1, Ordering::Relaxed);
        // Keep the shared slot for the `status` action's last-result poll, and
        // deliver THIS message's result to its own waiter via the per-message
        // oneshot (F061). A dropped receiver (waiter timed out / gave up) is fine.
        *last_result.write().await = Some(result_text.clone());
        if let Some(tx) = msg.respond_to {
            let _ = tx.send(result_text);
        }
        status.store(STATUS_IDLE, Ordering::Relaxed);
        result_notify.notify_one();

        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }
    tracing::debug!(agent = %engine.name(), "live agent processing loop exited");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a minimal `SessionAgentPool` with all agents already idle so
    /// eviction logic can treat it as a candidate.
    fn idle_pool(session_id: Uuid) -> SessionAgentPool {
        SessionAgentPool::new(session_id)
    }

    /// Fill the global pools map to `SESSION_AGENT_POOL_MAX` with idle pools,
    /// then call `insert_pool_with_cap` for a new session.  The map must still
    /// have exactly `SESSION_AGENT_POOL_MAX` entries (one was evicted).
    #[tokio::test]
    async fn eviction_triggers_at_cap() {
        let pools: tokio::sync::RwLock<HashMap<Uuid, SessionAgentPool>> =
            tokio::sync::RwLock::new(HashMap::new());

        // Fill to cap.
        {
            let mut w = pools.write().await;
            for _ in 0..SESSION_AGENT_POOL_MAX {
                let id = Uuid::new_v4();
                w.insert(id, idle_pool(id));
            }
        }
        assert_eq!(pools.read().await.len(), SESSION_AGENT_POOL_MAX);

        // Insert one more — should evict the oldest.
        let new_id = Uuid::new_v4();
        insert_pool_with_cap(&pools, new_id, idle_pool(new_id)).await;

        let map = pools.read().await;
        assert_eq!(
            map.len(),
            SESSION_AGENT_POOL_MAX,
            "pool count must remain at cap after eviction"
        );
        assert!(
            map.contains_key(&new_id),
            "the newly inserted pool must be present"
        );
    }

    /// Inserting below the cap must not evict anything.
    #[tokio::test]
    async fn no_eviction_below_cap() {
        let pools: tokio::sync::RwLock<HashMap<Uuid, SessionAgentPool>> =
            tokio::sync::RwLock::new(HashMap::new());

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        insert_pool_with_cap(&pools, id1, idle_pool(id1)).await;
        insert_pool_with_cap(&pools, id2, idle_pool(id2)).await;

        assert_eq!(pools.read().await.len(), 2);
    }
}
