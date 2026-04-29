use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{mpsc, Notify, RwLock};
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
    /// Callers waiting in `wait_for_agent_result` / `wait_until_idle` await
    /// this instead of polling — near-zero latency overhead.
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

/// Pool of always-alive agents for a single session.
pub struct SessionAgentPool {
    agents: HashMap<String, LiveAgent>,
    #[allow(dead_code)]
    session_id: Uuid,
}

impl SessionAgentPool {
    /// Creates a new empty pool for the given session.
    pub fn new(session_id: Uuid) -> Self {
        Self {
            agents: HashMap::new(),
            session_id,
        }
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
pub fn spawn_live_agent(
    name: String,
    engine: Arc<AgentEngine>,
    initial_task: String,
    session_id: Uuid,
) -> Option<LiveAgent> {
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
    ));

    // Send initial task synchronously — channel is fresh with capacity 32.
    if tx.try_send(AgentMessage { text: initial_task }).is_err() {
        task_handle.abort();
        return None;
    }

    Some(LiveAgent {
        name,
        message_tx: tx,
        status,
        last_result,
        cancel,
        created_at: Instant::now(),
        iteration_count,
        task_handle,
        result_notify,
    })
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
        // correct SessionAgentPool through enriched `_context`.

        let result = engine
            .run_subagent_with_session(
                &msg.text,
                max_iterations,
                deadline,
                Some(cancel.clone()),
                None,
                None,
                Some(session_id),
            )
            .await;

        let result_text = match result {
            Ok(text) => text,
            Err(e) => format!("Error: {e}"),
        };

        iteration_count.fetch_add(1, Ordering::Relaxed);
        *last_result.write().await = Some(result_text);
        status.store(STATUS_IDLE, Ordering::Relaxed);
        result_notify.notify_one();

        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }
    tracing::debug!(agent = %engine.name(), "live agent processing loop exited");
}
