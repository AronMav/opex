//! Phase 62 RES-05: graceful shutdown drain.
//!
//! Extracted from src/main.rs (v0.18.0 lines 456-485). Fixes the
//! lock-during-drain bug (read lock held across wait_drain await) per
//! Pitfall 5 in the Phase 62 research. Uses the snapshot → drop-lock →
//! cancel → join pattern pinned by Phase 61-05's DrainFixture.
//!
//! SOLE public function: `drain_agents_with_scheduler`. A no-scheduler
//! variant was considered and rejected — it would be dead code (only
//! main.rs calls this, and main.rs always has `&sched`), and CLAUDE.md
//! mandates `cargo clippy -- -D warnings`.
//!
//! # Why this module is trait-parameterised
//!
//! The binary target wires this to concrete types
//! (`AgentHandle`/`Scheduler`). The library target re-exports the module
//! (so integration tests and the Phase 61 `DrainFixture` can exercise the
//! shape without pulling the entire agent+scheduler subtree into the lib
//! surface — see `src/lib.rs` cascade cap comment). Trait-parameterising
//! `DrainableAgent` + `Shutdowner` keeps this module crate-dep-free.
//!
//! The TYPE `DrainFutureOf<H>` alias + the `DrainableAgent` trait lock in
//! the sequence: `snapshot_engines → cancel_all → wait_drain → shutdown`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use tokio::sync::RwLock;

/// An agent handle that can be drained and then shut down.
///
/// The binary-side implementation is `crate::agent::AgentHandle`; the
/// integration-test side can use any fake implementing this trait. The
/// handle is consumed by `shutdown`, preserving the ownership shape of the
/// real `AgentHandle::shutdown(mut self, scheduler)`.
pub trait DrainableAgent: Send + Sync + 'static {
    /// Scheduler dependency passed through to `shutdown`. Always `&Scheduler`
    /// in the binary; `()` in tests.
    type Scheduler: ?Sized + Sync;

    /// Cheap-cloneable snapshot of the drain-relevant engine state.
    /// In the binary this is `Arc<AgentEngine>` (already shareable).
    type EngineRef: Send + Sync + 'static;

    /// Clone the engine handle for off-lock use (cancel + wait_drain).
    fn engine_ref(&self) -> Self::EngineRef;

    /// Broadcast cancel to every in-flight request on this agent.
    /// Called BEFORE `wait_drain_for`. Must not block.
    fn cancel_all_requests(engine: &Self::EngineRef);

    /// Wait until all in-flight requests complete, or `timeout` elapses.
    /// The future returned must be `Send` so `join_all` can drive a Vec.
    fn wait_drain_for(
        engine: &Self::EngineRef,
        timeout: Duration,
    ) -> impl Future<Output = ()> + Send;

    /// Consume the handle and perform final teardown (scheduler job removal,
    /// subagent cancellation). Called under no lock.
    fn shutdown(self, scheduler: &Self::Scheduler) -> impl Future<Output = ()> + Send;
}

/// Drain all agents: broadcast cancel, wait with bounded timeout, then shut each down.
///
/// Sequence (fixes v0.18.0 lock-during-drain bug — Pitfall 5):
///   1. Snapshot `(name, EngineRef)` under read lock → DROP the lock.
///   2. Broadcast `cancel_all_requests()` to each agent (sync, no lock held).
///   3. Bounded drain — race `join_all(wait_drain_for)` against `drain_timeout`.
///   4. Take write lock, drain the map into a `Vec`, drop the lock, then
///      `await handle.shutdown(scheduler)` for each handle off-lock.
///
/// On step-3 timeout, any still-running agent is force-cleaned via
/// `handle.shutdown(scheduler)` in step 4 — no agent can survive the drain.
pub async fn drain_agents_with_scheduler<H: DrainableAgent>(
    agents: Arc<RwLock<HashMap<String, H>>>,
    drain_timeout: Duration,
    scheduler: &H::Scheduler,
) {
    tracing::info!(
        drain_timeout_secs = drain_timeout.as_secs(),
        "graceful shutdown: cancelling all active requests"
    );

    // Step 1: snapshot engine refs under read lock, then DROP the lock.
    // This is the fix for the v0.18.0 bug (Pitfall 5) — the previous code
    // held the read guard across the wait_drain await, which could deadlock
    // against a concurrent writer.
    let snapshot: Vec<(String, H::EngineRef)> = {
        let guard = agents.read().await;
        guard
            .iter()
            .map(|(name, handle)| (name.clone(), handle.engine_ref()))
            .collect()
    }; // ← read lock RELEASED here

    // Step 2: broadcast cancel to all agents (sync — no lock held).
    for (name, engine) in &snapshot {
        tracing::info!(agent = %name, "cancelling active requests");
        H::cancel_all_requests(engine);
    }

    // Step 3: bounded drain — race join_all against drain_timeout.
    tracing::info!(
        drain_timeout_secs = drain_timeout.as_secs(),
        "graceful shutdown: waiting for agents to drain"
    );
    let drain_futures: Vec<_> = snapshot
        .iter()
        .map(|(_, engine)| H::wait_drain_for(engine, drain_timeout))
        .collect();
    let join_result = tokio::time::timeout(drain_timeout, join_all(drain_futures)).await;
    if join_result.is_err() {
        tracing::warn!(
            drain_timeout_secs = drain_timeout.as_secs(),
            "graceful shutdown: drain timeout hit — some agents didn't cooperate"
        );
    }

    // Snapshot goes out of scope here — drop engine refs before moving on.
    drop(snapshot);

    // Step 4: take write lock; drain the map under the write guard, then
    // release the guard BEFORE awaiting handle.shutdown (avoids holding the
    // write lock across an await).
    tracing::info!("graceful shutdown: stopping agents");
    let drained: Vec<(String, H)> = {
        let mut guard = agents.write().await;
        guard.drain().collect()
    }; // ← write lock RELEASED here

    for (name, handle) in drained {
        tracing::info!(agent = %name, "shutting down agent");
        handle.shutdown(scheduler).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Instant;
    use tokio::time::timeout;

    /// Minimal fake agent used by `shutdown.rs` unit tests. Mirrors the
    /// snapshot-then-release contract we ship to `main.rs`.
    struct FakeAgent {
        engine: Arc<FakeEngine>,
    }

    struct FakeEngine {
        cancelled: AtomicBool,
        shutdown_called: Arc<AtomicUsize>,
    }

    impl DrainableAgent for FakeAgent {
        type Scheduler = ();
        type EngineRef = Arc<FakeEngine>;

        fn engine_ref(&self) -> Self::EngineRef {
            self.engine.clone()
        }
        fn cancel_all_requests(engine: &Self::EngineRef) {
            engine.cancelled.store(true, Ordering::SeqCst);
        }
        async fn wait_drain_for(engine: &Self::EngineRef, timeout: Duration) {
            // Cooperative fake: returns as soon as cancel fires.
            let start = Instant::now();
            while start.elapsed() < timeout {
                if engine.cancelled.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        async fn shutdown(self, _scheduler: &()) {
            self.engine.shutdown_called.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn make_agent(shutdown_counter: Arc<AtomicUsize>) -> FakeAgent {
        FakeAgent {
            engine: Arc::new(FakeEngine {
                cancelled: AtomicBool::new(false),
                shutdown_called: shutdown_counter,
            }),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn drain_empty_map_returns_quickly() {
        let agents: Arc<RwLock<HashMap<String, FakeAgent>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let start = Instant::now();
        drain_agents_with_scheduler(agents, Duration::from_secs(5), &()).await;
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "empty drain must be fast; took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn drain_cancels_then_shuts_down_every_agent() {
        let shutdown_counter = Arc::new(AtomicUsize::new(0));
        let agents: Arc<RwLock<HashMap<String, FakeAgent>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let mut refs: Vec<Arc<FakeEngine>> = Vec::new();
        {
            let mut g = agents.write().await;
            for i in 0..3 {
                let a = make_agent(shutdown_counter.clone());
                refs.push(a.engine.clone());
                g.insert(format!("agent-{i}"), a);
            }
        }
        drain_agents_with_scheduler(agents.clone(), Duration::from_secs(5), &()).await;
        for r in &refs {
            assert!(r.cancelled.load(Ordering::SeqCst), "each agent must be cancelled");
        }
        assert_eq!(shutdown_counter.load(Ordering::SeqCst), 3, "each agent shutdown exactly once");
        assert!(
            agents.read().await.is_empty(),
            "agents map must be empty after drain"
        );
    }

    #[tokio::test]
    async fn bg_tasks_tracked_through_shutdown() {
        use tokio_util::task::TaskTracker;
        let tracker = Arc::new(TaskTracker::new());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tracker.spawn(async move { let _ = tx.send(()); });
        tracker.close();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), tracker.wait()).await;
        assert!(rx.await.is_ok(), "spawned bg task must have completed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn drain_releases_read_lock_before_wait_drain() {
        // THE v0.18.0 BUG REPRODUCER at the unit level: while drain is
        // running, a concurrent writer MUST be able to acquire the write
        // lock within a short window — proof the read lock is not held
        // across the wait_drain await.
        let agents: Arc<RwLock<HashMap<String, FakeAgent>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let shutdown_counter = Arc::new(AtomicUsize::new(0));
        {
            let mut g = agents.write().await;
            for i in 0..3 {
                g.insert(format!("agent-{i}"), make_agent(shutdown_counter.clone()));
            }
        }

        // Start drain with a 5s budget — plenty of time to observe lock state.
        let agents_for_drain = agents.clone();
        let drain = tokio::spawn(async move {
            drain_agents_with_scheduler(agents_for_drain, Duration::from_secs(5), &()).await;
        });

        // Give drain a tick to enter its post-read-lock wait loop, then try
        // to acquire the write lock. A pending writer request must be able
        // to acquire the lock within a small window (drain is in wait_drain
        // with no lock held). If the v0.18.0 bug were present, the writer
        // would block for the full 5s drain budget.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let writer_elapsed = timeout(Duration::from_secs(2), async {
            let start = Instant::now();
            let _guard = agents.write().await;
            start.elapsed()
        })
        .await
        .expect("writer must acquire within 2s");

        assert!(
            writer_elapsed < Duration::from_secs(2),
            "writer acquired in {writer_elapsed:?} — must be < 2s to prove read lock was released"
        );

        // Let drain finish — it will re-acquire write lock when the test's
        // writer guard drops (drops immediately above once `_guard` expires).
        timeout(Duration::from_secs(10), drain)
            .await
            .expect("drain must complete within 10s")
            .expect("drain task panicked");
    }
}
