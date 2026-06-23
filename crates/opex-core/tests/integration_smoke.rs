//! Smoke integration test for TEST-01.
//!
//! Verifies the TestHarness actually spins up a real PostgreSQL container,
//! runs every migration, and that two harnesses can coexist without a port clash.
//!
//! Requires a running Docker daemon. CI matrix (Plan 06) provides this.

mod support;

use std::time::Duration;
use support::TestHarness;
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_test_01_harness_smoke() {
    timeout(Duration::from_secs(30), async {
        let harness = TestHarness::new()
            .await
            .expect("TestHarness::new must succeed when Docker is available");

        // Sanity: at least one well-known production table exists.
        // Pick `sessions` because it ships in migration 002 (multi_agent_sessions)
        // — present since the very first milestone, will not be renamed.
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'sessions')",
        )
        .fetch_one(harness.pool())
        .await
        .expect("query against ephemeral PG must succeed");

        assert!(
            exists,
            "expected production table `sessions` to exist after migrations"
        );
    })
    .await
    .expect("smoke test exceeded 30s — Docker missing or migrations stuck");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_test_01_two_harnesses_use_distinct_ports() {
    timeout(Duration::from_secs(45), async {
        // CONCURRENT startup via tokio::join! — total time ≈ slower of the two
        // (~15-20s on a warm Docker host) instead of sequential ~30-40s. Keeps
        // us comfortably inside the 45s outer timeout.
        let (a_res, b_res) = tokio::join!(TestHarness::new(), TestHarness::new());
        let a = a_res.expect("first harness");
        let b = b_res.expect("second harness");
        assert_ne!(
            a.pg_url(),
            b.pg_url(),
            "two harnesses must run on independent host ports"
        );
    })
    .await
    .expect("two-harness test exceeded 45s — port allocation hung");
}
