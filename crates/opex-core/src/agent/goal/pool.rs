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

/// Sweep the goal-locks map once it grows past this many entries (F037).
const GOAL_LOCKS_SWEEP_THRESHOLD: usize = 256;

/// Per-session lock that the driver and user-message entry points share.
pub fn goal_lock(locks: &GoalLocks, session_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    let lock = locks
        .entry(session_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    // F037: reclaim idle entries so the map doesn't grow unbounded (one entry
    // per distinct session, previously never removed). An entry whose Arc has
    // `strong_count == 1` is held ONLY by the map — no user turn or driver
    // holds it — so dropping it is safe. The entry we just cloned above has
    // strong_count >= 2, so it is never swept out from under this caller.
    if locks.len() > GOAL_LOCKS_SWEEP_THRESHOLD {
        locks.retain(|_, v| Arc::strong_count(v) > 1);
    }
    lock
}

/// Acquire the serialization guard a user turn holds against the goal driver.
///
/// FIX C2: the guard is taken UNCONDITIONALLY whenever goal locks are configured
/// — independent of goal-driver *pool membership*. A driver task is spawned and
/// begins running (acquiring `goal_lock` for its first turn) BEFORE the caller
/// registers its handle in the pool (`resume_autonomous_goals`,
/// `bootstrap_cron_goal`, `/goal`). Gating the user guard on `is_running` left a
/// TOCTOU window in which a user turn saw an empty pool, skipped the lock, and
/// ran concurrently with the driver's first turn on the same session. Returns
/// `None` only when the engine has no goal infrastructure (`locks` is `None`).
pub async fn user_turn_goal_guard(
    locks: Option<&GoalLocks>,
    session_id: Uuid,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    match locks {
        Some(l) => Some(goal_lock(l, session_id).lock_owned().await),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn f037_goal_lock_reclaims_idle_but_keeps_held() {
        let locks = new_locks();
        let held_id = Uuid::new_v4();
        let _held = goal_lock(&locks, held_id); // strong_count 2 (map + this)

        // Churn well past the sweep threshold with idle (immediately-dropped) locks.
        for _ in 0..(GOAL_LOCKS_SWEEP_THRESHOLD * 2) {
            let _ = goal_lock(&locks, Uuid::new_v4());
        }

        // The held lock survived every sweep …
        assert!(locks.contains_key(&held_id), "a held goal lock must not be swept");
        // … and the map stayed bounded rather than growing per distinct session.
        assert!(
            locks.len() <= GOAL_LOCKS_SWEEP_THRESHOLD + 2,
            "idle goal locks must be reclaimed (len={})",
            locks.len()
        );
    }

    #[tokio::test]
    async fn user_turn_guard_serializes_behind_driver_even_when_pool_empty() {
        let locks = new_locks();
        let pool = new_pool(); // empty: simulates the spawn window (driver running, not yet inserted)
        let sid = Uuid::new_v4();

        // The driver holds its per-turn guard (the same lock the driver loop takes).
        let driver_guard = goal_lock(&locks, sid).lock_owned().await;

        // Precondition: the driver is NOT observable via the pool yet.
        assert!(!is_running(&pool, sid), "spawn window: driver not yet registered in pool");

        // A concurrent user turn must still serialize behind the driver.
        let locks2 = locks.clone();
        let user = tokio::spawn(async move { user_turn_goal_guard(Some(&locks2), sid).await });

        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(
            !user.is_finished(),
            "user turn must block behind the driver guard despite the empty pool (FIX C2)"
        );

        drop(driver_guard);
        let guard = tokio::time::timeout(Duration::from_secs(1), user)
            .await
            .expect("user turn must acquire once the driver releases")
            .unwrap();
        assert!(guard.is_some(), "user turn acquired the goal guard after release");
    }

    #[tokio::test]
    async fn user_turn_guard_is_none_without_locks() {
        // No goal infrastructure (e.g. configs without goal support) → no guard, no-op.
        assert!(user_turn_goal_guard(None, Uuid::new_v4()).await.is_none());
    }
}
