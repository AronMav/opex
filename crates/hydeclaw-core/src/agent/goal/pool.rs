//! Per-session goal-driver registry + the goal/user serialization lock.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// (channel, chat_id) the driver delivers to; `None` for web sessions.
pub type GoalTarget = Option<(String, String)>;

pub struct GoalDriverHandle {
    pub cancel: CancellationToken,
    pub join: JoinHandle<()>,
    pub target: GoalTarget,
}

pub type GoalDriverPool = Arc<DashMap<Uuid, GoalDriverHandle>>;
pub type GoalLocks = Arc<DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>;

pub fn new_pool() -> GoalDriverPool {
    Arc::new(DashMap::new())
}

pub fn new_locks() -> GoalLocks {
    Arc::new(DashMap::new())
}

pub fn is_running(pool: &GoalDriverPool, session_id: Uuid) -> bool {
    pool.get(&session_id).map(|h| !h.join.is_finished()).unwrap_or(false)
}

pub fn stop(pool: &GoalDriverPool, session_id: Uuid) {
    if let Some((_, h)) = pool.remove(&session_id) {
        h.cancel.cancel();
        h.join.abort();
    }
}

/// Per-session lock that the driver and user-message entry points share.
pub fn goal_lock(locks: &GoalLocks, session_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    locks
        .entry(session_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}
