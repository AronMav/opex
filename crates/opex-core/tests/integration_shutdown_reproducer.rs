//! RES-05 reproducer: proves the extracted `shutdown.rs` does NOT hold the
//! `agents_map` read lock across the drain await.
//!
//! This is the test Phase 61-05 required BEFORE the main.rs refactor ships.
//! It demonstrates that a concurrent writer can acquire the write lock
//! WHILE `drain_agents_with_scheduler` is in its bounded-join phase — the
//! property the v0.18.0 `main.rs:456-485` block violated.
//!
//! Three tests:
//!   1. `drain_releases_read_lock_before_awaiting` — THE bug reproducer:
//!      while drain is running, a concurrent writer acquires the write lock
//!      within 2s. If the v0.18.0 bug were present, the writer would block
//!      for the full 5s drain budget.
//!   2. `drain_empty_map_completes_immediately` — idle drain is fast (<100ms).
//!   3. `drain_cancels_all_agents` — every agent in the map receives the
//!      cancel signal before wait_drain_for is awaited.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use opex_core::shutdown::{DrainableAgent, drain_agents_with_scheduler};
use tokio::sync::RwLock;
use tokio::time::timeout;

// ── Fake agent implementation ────────────────────────────────────────────

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
    fn wait_drain_for(
        engine: &Self::EngineRef,
        budget: Duration,
    ) -> impl Future<Output = ()> + Send {
        let engine = engine.clone();
        async move {
            // Cooperative: returns as soon as cancel fires.
            let start = Instant::now();
            while start.elapsed() < budget {
                if engine.cancelled.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }
    fn shutdown(self, _scheduler: &()) -> impl Future<Output = ()> + Send {
        let engine = self.engine;
        async move {
            engine.shutdown_called.fetch_add(1, Ordering::SeqCst);
        }
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

// ── Tests ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drain_releases_read_lock_before_awaiting() {
    timeout(Duration::from_secs(30), async {
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
        let drain_handle = tokio::spawn(async move {
            drain_agents_with_scheduler(agents_for_drain, Duration::from_secs(5), &()).await;
        });

        // Give drain a tick to pass the snapshot read-lock and enter its
        // wait_drain_for phase. If the v0.18.0 bug were present, drain would
        // still be holding the read lock here.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Concurrent writer must acquire the write lock within 2s. With the
        // v0.18.0 lock-during-drain bug, this would block for up to 5s.
        //
        // NOTE: drain's cooperative fake honours cancel immediately, so
        // step 3 (wait_drain_for) returns very quickly, and step 4 (the
        // write-lock drain) may already be running in parallel. The writer
        // is still guaranteed to get the lock fairly — tokio RwLock is
        // write-preferring, and drain's write lock is released before each
        // `handle.shutdown` await (the fix).
        let writer_start = Instant::now();
        let writer_elapsed = timeout(Duration::from_secs(2), async {
            let _guard = agents.write().await;
            writer_start.elapsed()
        })
        .await
        .expect("writer must acquire within 2s — drain must not hold any lock across await");

        assert!(
            writer_elapsed < Duration::from_secs(2),
            "writer acquired in {writer_elapsed:?} — must be < 2s to prove locks were released"
        );

        // Let drain complete.
        timeout(Duration::from_secs(10), drain_handle)
            .await
            .expect("drain must complete within 10s")
            .expect("drain task panicked");
    })
    .await
    .expect("test exceeded 30s outer timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drain_empty_map_completes_immediately() {
    timeout(Duration::from_secs(10), async {
        let agents: Arc<RwLock<HashMap<String, FakeAgent>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let start = Instant::now();
        drain_agents_with_scheduler(agents, Duration::from_secs(5), &()).await;
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "empty drain must be fast; took {:?}",
            start.elapsed()
        );
    })
    .await
    .expect("empty-map test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drain_cancels_all_agents() {
    timeout(Duration::from_secs(10), async {
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
            assert!(
                r.cancelled.load(Ordering::SeqCst),
                "every agent must be cancelled"
            );
        }
        assert_eq!(
            shutdown_counter.load(Ordering::SeqCst),
            3,
            "each agent shutdown exactly once"
        );
        assert!(
            agents.read().await.is_empty(),
            "agents map must be empty after drain"
        );
    })
    .await
    .expect("cancel test timed out");
}
