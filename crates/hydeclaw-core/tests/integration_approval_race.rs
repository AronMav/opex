//! TEST-03: Characterization of approval-resolve race semantics.
//!
//! Asserts: when 100 concurrent callers attempt to resolve the SAME approval id,
//! exactly one wins and 99 lose. Wall time MUST be < 1 s.
//!
//! Why this test exists:
//! - REF-02 (Phase 66) swapped RwLock<HashMap> for DashMap in approval_manager.
//! - DATA-04 (Phase 63) tightened transactional isolation via SELECT FOR UPDATE
//!   and replaced the `Result<bool>` shim with the typed `resolve_approval_strict`.
//! - TIER 2.1 removed the legacy `resolve_approval` `Result<bool>` wrapper; the
//!   tests below now call `resolve_approval_strict` directly and translate the
//!   typed error variants to the same race-outcome contract.
//! - This test pins the OBSERVABLE behavior so future changes can't silently
//!   regress the exactly-once contract.
//!
//! Requires a functional Docker daemon for the ephemeral PostgreSQL container —
//! CI matrix (Plan 06) provides this.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use hydeclaw_core::db::approvals::{create_approval, resolve_approval_strict, ApprovalError};
use serde_json::json;
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_test_03_approval_race_exactly_once_db_layer() {
    timeout(Duration::from_secs(30), async {
        let harness = TestHarness::new()
            .await
            .expect("ephemeral PG must come up");
        let pool = harness.pool().clone();

        // 1. Create a single approval row to race on.
        let approval_id: Uuid = create_approval(
            &pool,
            "char-test-agent",
            None,
            "noop_tool",
            &json!({}),
            &json!({}),
        )
        .await
        .expect("create_approval must succeed against migrated schema");

        // 2. Race: 100 concurrent resolve_approval_strict calls.
        //
        // `flavor = "multi_thread", worker_threads = 4` is REQUIRED — a single-
        // threaded runtime would serialize the 100 tasks and defeat the race.
        const N: usize = 100;
        let pool = Arc::new(pool);
        let mut handles = Vec::with_capacity(N);
        let start = Instant::now();
        for i in 0..N {
            let pool = Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                resolve_approval_strict(
                    &pool,
                    approval_id,
                    "approved",
                    &format!("racer-{i}"),
                )
                .await
            }));
        }

        // 3. Collect outcomes — translate typed errors to the legacy
        //    win/loss/error contract that this characterization test pins.
        let mut wins = 0usize;
        let mut losses = 0usize;
        let mut errors = 0usize;
        for h in handles {
            match h.await.expect("task panicked") {
                Ok(()) => wins += 1,
                Err(ApprovalError::AlreadyResolved { .. })
                | Err(ApprovalError::NotFound { .. }) => losses += 1,
                Err(ApprovalError::Db(_)) => errors += 1,
            }
        }
        let elapsed = start.elapsed();

        // 4. Assertions: characterize the baseline behavior.
        assert_eq!(
            errors, 0,
            "no DB errors expected on baseline; got {errors} errors"
        );
        assert_eq!(
            wins, 1,
            "exactly ONE racer must transition pending→resolved; got {wins}"
        );
        assert_eq!(
            losses,
            N - 1,
            "remaining racers must observe AlreadyResolved; got {losses}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "100-task race must complete < 1s; took {elapsed:?}"
        );

        // Observability: surfaces wall-clock time when invoked with --nocapture
        // so future runs can be compared (recorded in 61-03-SUMMARY.md).
        eprintln!(
            "test_test_03_approval_race_exactly_once_db_layer: \
             wins={wins} losses={losses} errors={errors} elapsed={elapsed:?}"
        );
    })
    .await
    .expect("approval race test exceeded 30s wall timeout");
}

/// Sensitivity probe: double-resolve of the SAME existing approval.
///
/// Creates one approval row, then sequentially calls `resolve_approval_strict`
/// TWICE. Asserts: first call returns `Ok(())` (transitioned pending → approved),
/// second call returns `Err(AlreadyResolved)` (the SELECT FOR UPDATE sees the
/// new status). This proves the row-state path is the gating factor: if the
/// test ever observed two `Ok(())` it would mean the status check was removed
/// and the exactly-once race assertion would degenerate to a tautology.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_test_03_approval_race_sensitivity_double_resolve_loses_second() {
    timeout(Duration::from_secs(30), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool().clone();

        let approval_id: Uuid = create_approval(
            &pool,
            "char-test-agent",
            None,
            "noop_tool",
            &json!({}),
            &json!({}),
        )
        .await
        .expect("create_approval must succeed");

        resolve_approval_strict(&pool, approval_id, "approved", "first")
            .await
            .expect("first resolve_approval_strict against pending row must return Ok(())");

        let second = resolve_approval_strict(&pool, approval_id, "approved", "second").await;
        match second {
            Err(ApprovalError::AlreadyResolved { .. }) => {}
            other => panic!(
                "second resolve_approval_strict against already-resolved row must return \
                 Err(AlreadyResolved); got {other:?} — the WHERE status='pending' check may \
                 have been removed, which would degenerate the exactly-once race assertion \
                 to a tautology"
            ),
        }
    })
    .await
    .expect("sensitivity test exceeded 30s");
}

/// Phase 63 DATA-04 extension: 100-task race against `resolve_approval_strict`.
///
/// Preserves the Phase 61-03 contract (legacy tests above are untouched) AND adds
/// a typed-error assertion block: winners get `Ok(())`, losers get
/// `Err(ApprovalError::AlreadyResolved { .. })`, zero `Err(Db(_))`. Wall time
/// MUST be < 1 s to prove `FOR UPDATE` doesn't serialise pathologically.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strict_race_exactly_one_ok_rest_already_resolved() {
    use hydeclaw_core::db::approvals::{resolve_approval_strict, ApprovalError};
    timeout(Duration::from_secs(30), async {
        let harness = TestHarness::new().await.expect("PG");
        let pool = harness.pool().clone();

        let approval_id: Uuid = create_approval(
            &pool,
            "char-test-agent",
            None,
            "noop_tool",
            &json!({}),
            &json!({}),
        )
        .await
        .expect("create_approval");

        const N: usize = 100;
        let pool = Arc::new(pool);
        let mut handles = Vec::with_capacity(N);
        let start = Instant::now();
        for i in 0..N {
            let pool = Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                resolve_approval_strict(
                    &pool,
                    approval_id,
                    "approved",
                    &format!("strict-racer-{i}"),
                )
                .await
            }));
        }

        let mut wins = 0usize;
        let mut already_resolved = 0usize;
        let mut not_found = 0usize;
        let mut db_errors = 0usize;
        for h in handles {
            match h.await.expect("task panicked") {
                Ok(()) => wins += 1,
                Err(ApprovalError::AlreadyResolved { .. }) => already_resolved += 1,
                Err(ApprovalError::NotFound { .. }) => not_found += 1,
                Err(ApprovalError::Db(_)) => db_errors += 1,
            }
        }
        let elapsed = start.elapsed();

        assert_eq!(db_errors, 0, "zero DB errors expected; got {db_errors}");
        assert_eq!(
            not_found, 0,
            "zero NotFound expected — the row exists; got {not_found}"
        );
        assert_eq!(wins, 1, "exactly one Ok(()) must win; got {wins}");
        assert_eq!(
            already_resolved,
            N - 1,
            "remaining must be AlreadyResolved; got {already_resolved}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "strict 100-task race must complete < 1s; took {elapsed:?}"
        );
        eprintln!(
            "strict_race_exactly_one_ok_rest_already_resolved: \
             wins={wins} already_resolved={already_resolved} \
             db_errors={db_errors} elapsed={elapsed:?}"
        );
    })
    .await
    .expect("strict race exceeded 30s");
}
