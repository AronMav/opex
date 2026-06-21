//! Per-session goal-driver registry + the goal/user serialization lock.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// (channel, chat_id) the driver delivers to; `None` for web sessions.
pub type GoalTarget = Option<(String, i64)>;

pub struct GoalDriverHandle {
    pub cancel: CancellationToken,
    pub join: JoinHandle<()>,
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

/// Grace window for a goal turn to wind down cooperatively before a hard abort.
const GOAL_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Stop the goal driver for a session cooperatively.
///
/// R-GOAL fix: previously this called `h.join.abort()` immediately after
/// `cancel()`. Aborting drops the in-flight turn's `SessionLifecycleGuard`
/// while it is still `Running`, so the guard's `Drop` marks the user's chat
/// session `'failed'` — a normal `/goal stop|clear|<new>` silently killed the
/// conversation. Now we only `cancel()` the token (which propagates into
/// `run_goal_turn` → `execute()`, letting the turn reach `finalize` and mark
/// the session `done`/`interrupted`), and a detached backstop hard-aborts only
/// if the turn ignores the token past the grace window (sync wedge).
pub fn stop(pool: &GoalDriverPool, session_id: Uuid) {
    if let Some((_, h)) = pool.remove(&session_id) {
        let GoalDriverHandle { cancel, mut join } = h;
        cancel.cancel();
        tokio::spawn(async move {
            if tokio::time::timeout(GOAL_STOP_GRACE, &mut join).await.is_err() {
                // Turn ignored the cancel token past the grace window — abort as
                // a last resort so the task / lock are freed. This is the only
                // path that can still produce a guard-drop 'failed', and only
                // for a genuinely wedged turn.
                join.abort();
            }
        });
    }
}

/// Per-session lock that the driver and user-message entry points share.
pub fn goal_lock(locks: &GoalLocks, session_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    locks
        .entry(session_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}
