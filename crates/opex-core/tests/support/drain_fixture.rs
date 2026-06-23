//! In-process drain harness modeling the TARGET (post-Phase-62-RES-05) shape
//! of the src/main.rs shutdown sequence.
//!
//! IMPORTANT — what this fixture pins:
//!   The TARGET pattern: snapshot tasks under lock, drop the lock, then
//!   cancel + join the snapshot. This is the shape Phase 62 RES-05 will
//!   retrofit into main.rs.
//!
//! IMPORTANT — what this fixture does NOT pin:
//!   The CURRENT v0.18.0 main.rs uses lock-during-drain (read lock held
//!   across the wait_drain in step 2, then upgraded to write lock in step
//!   3). That bug cannot be reproduced from the integration-test boundary
//!   without extracting main.rs's shutdown logic. Phase 62 RES-05 plan
//!   must include a separate reproducer test (against the extracted
//!   shutdown.rs) BEFORE refactoring main.rs.
//!
//! The sensitivity probe (test_test_05..._sensitivity_uncooperative...)
//! ensures the timing assertions have teeth — if Phase 62 RES-05 silently
//! removes the drain timeout, the sensitivity test will fail.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct FixtureTask {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

pub struct DrainFixture {
    tasks: Arc<Mutex<Vec<FixtureTask>>>,
}

impl DrainFixture {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Spawn a task that respects cancellation. Sleeps up to `work_duration`,
    /// but exits early as soon as the cancel token fires.
    pub async fn spawn_cooperative(&self, work_duration: Duration) {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(work_duration) => {}
                _ = cancel_clone.cancelled() => {}
            }
        });
        self.tasks.lock().await.push(FixtureTask { handle, cancel });
    }

    /// Spawn a task that ignores cancellation — sleeps for the full duration.
    /// Used by the sensitivity probe to prove drain budget is real.
    pub async fn spawn_uncooperative(&self, work_duration: Duration) {
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(work_duration).await;
        });
        self.tasks.lock().await.push(FixtureTask { handle, cancel });
    }

    /// Number of tasks not yet completed.
    pub async fn active_count(&self) -> usize {
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|t| !t.handle.is_finished());
        tasks.len()
    }

    /// Execute the TARGET shutdown sequence (snapshot → cancel → bounded join).
    ///
    /// Phase 62 RES-05 will retrofit this exact shape into src/main.rs:
    ///
    /// 1. SNAPSHOT under lock, then DROP the lock — no lock held across wait.
    /// 2. Signal cancel to every task in the snapshot.
    /// 3. Race join_all against the drain timeout.
    ///
    /// Returns wall-clock time the full sequence took.
    pub async fn shutdown(&self, drain_timeout: Duration) -> Duration {
        let start = Instant::now();

        // 1. Snapshot tasks by clone, drop the lock, then signal cancel.
        //    THE TARGET PATTERN — main.rs today still holds the read lock
        //    across the wait. We model the SAFER pattern so the test stays
        //    valid post-refactor.
        let snapshot: Vec<(JoinHandle<()>, CancellationToken)> = {
            let mut taken = self.tasks.lock().await;
            std::mem::take(&mut *taken)
                .into_iter()
                .map(|t| (t.handle, t.cancel))
                .collect()
        };

        for (_handle, cancel) in &snapshot {
            cancel.cancel();
        }

        // 2. Bounded drain — race join_all against the drain timeout.
        let join_futures = snapshot.into_iter().map(|(handle, _)| async move {
            let _ = handle.await;
        });
        let _ = tokio::time::timeout(
            drain_timeout,
            futures_util::future::join_all(join_futures),
        )
        .await;

        // 3. (Bookkeeping only — JoinHandle abort happens implicitly when
        //    the handle is dropped; no extra step required.)

        start.elapsed()
    }
}

impl Default for DrainFixture {
    fn default() -> Self {
        Self::new()
    }
}
