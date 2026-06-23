//! TEST-05: Characterization of the TARGET shutdown drain shape (post-Phase-62-RES-05).
//!
//! Models the snapshot→cancel→join sequence Phase 62 RES-05 will retrofit
//! into src/main.rs. Three baseline tests pin expected timings; the fourth
//! is a sensitivity probe that proves the timing assertions have teeth.
//!
//! KNOWN LIMITATION: this characterizes the TARGET shape, not in-flight v0.18.0
//! main.rs (which uses lock-during-drain). See plan-level <deferred> block —
//! Phase 62 RES-05 plan must add a separate reproducer test against extracted
//! shutdown.rs BEFORE refactoring main.rs.

mod support;

use std::time::Duration;
use support::DrainFixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_test_05_shutdown_drain_idle_completes_immediately() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let fixture = DrainFixture::new();
        let elapsed = fixture.shutdown(Duration::from_secs(10)).await;
        assert!(
            elapsed < Duration::from_millis(100),
            "idle drain must complete in < 100ms; took {elapsed:?}"
        );
    })
    .await
    .expect("idle drain test exceeded 60s outer timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_test_05_shutdown_drain_three_cooperative_tasks_within_budget() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let fixture = DrainFixture::new();
        for _ in 0..3 {
            fixture.spawn_cooperative(Duration::from_secs(30)).await;
        }
        let elapsed = fixture.shutdown(Duration::from_secs(10)).await;
        assert!(
            elapsed < Duration::from_secs(1),
            "3 cooperative tasks must drain in < 1s when they honour cancel; took {elapsed:?}"
        );
    })
    .await
    .expect("cooperative-3 drain test exceeded 60s outer timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_test_05_shutdown_drain_uncooperative_task_hits_budget_ceiling() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let fixture = DrainFixture::new();
        // Sleeps 30s, ignores cancel. Drain budget is 2s — the budget MUST hold,
        // not the 30s task duration.
        fixture.spawn_uncooperative(Duration::from_secs(30)).await;
        let elapsed = fixture.shutdown(Duration::from_secs(2)).await;
        assert!(
            elapsed >= Duration::from_millis(1500) && elapsed <= Duration::from_millis(2700),
            "uncooperative task must hit drain ceiling near 2s, not full 30s; got {elapsed:?}"
        );
    })
    .await
    .expect("ceiling test exceeded 60s outer timeout");
}

/// Sensitivity probe: proves the drain budget is the gating factor, not task duration.
///
/// If the drain logic ever silently joined the uncooperative task to completion
/// (e.g. by removing the timeout), this test would take ~5s instead of ~500ms.
/// Today, on the target-shape fixture, this MUST observe the budget firing
/// well below the 5s task duration.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_test_05_shutdown_drain_sensitivity_uncooperative_exceeds_short_budget() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let fixture = DrainFixture::new();
        fixture.spawn_uncooperative(Duration::from_secs(5)).await;

        let elapsed = fixture.shutdown(Duration::from_millis(500)).await;
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed <= Duration::from_millis(900),
            "drain budget must fire near 500ms, not 5s; got {elapsed:?}"
        );
        // The elapsed bound above proves the task did not run to its 5s natural end.
    })
    .await
    .expect("sensitivity test exceeded 30s outer timeout");
}
