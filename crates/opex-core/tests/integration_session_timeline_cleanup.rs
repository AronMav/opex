//! RES-03 (Phase 62): verify batched DELETE `prune_old_events_batched` against a real PG (session timeline).
//!
//! Tests validate the wrapped-SELECT-with-LIMIT pattern PostgreSQL requires
//! (no native `DELETE ... LIMIT`). See `.planning/phases/62-resilience/62-RESEARCH.md`
//! Pattern 3 + Pitfall 2 for background.

mod support;

use std::time::Duration;

use opex_core::db::session_timeline::prune_old_events_batched;
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

/// Insert a session row (FK target for session_timeline) and return its id.
async fn insert_session(pool: &sqlx::PgPool) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sessions (agent_id, user_id, channel)
        VALUES ('test-agent', 'test-user', 'test-channel')
        RETURNING id
        "#,
    )
    .fetch_one(pool)
    .await
    .expect("insert session");
    id
}

/// Seed `count` events aged `age_days` days old. Uses a shared session to avoid
/// FK churn — `session_timeline.session_id` references `sessions.id` ON DELETE CASCADE.
async fn seed_events(pool: &sqlx::PgPool, count: usize, age_days: i32) {
    let session_id = insert_session(pool).await;
    for i in 0..count {
        sqlx::query(
            r#"
            INSERT INTO session_timeline (session_id, event_type, payload, created_at)
            VALUES ($1, 'tool_end', NULL, now() - make_interval(days => $2, secs => $3))
            "#,
        )
        .bind(session_id)
        .bind(age_days)
        .bind(i as i32)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("seed event {i} failed: {e}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prune_batched_deletes_only_old_rows() {
    timeout(Duration::from_secs(180), async {
        let harness = TestHarness::new().await.expect("spawn PG");
        let pool = harness.pool();

        // Seed 100 old (age 10 days) + 10 fresh (age 1 day).
        seed_events(pool, 100, 10).await;
        seed_events(pool, 10, 1).await;

        // Retention 7 days, batch 50 → loops twice (50 + 50) + terminates on empty.
        let deleted = prune_old_events_batched(pool, 7, 50)
            .await
            .expect("prune_old_events_batched");
        assert_eq!(deleted, 100, "only the 100 old rows must be deleted");

        let remaining: (i64,) =
            sqlx::query_as("SELECT COUNT(*)::bigint FROM session_timeline")
                .fetch_one(pool)
                .await
                .expect("count remaining");
        assert_eq!(remaining.0, 10, "10 fresh rows must survive");
    })
    .await
    .expect("prune_batched_deletes_only_old_rows exceeded 180s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prune_batched_zero_days_is_noop() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new().await.expect("spawn PG");
        let pool = harness.pool();

        seed_events(pool, 50, 10).await;

        let deleted = prune_old_events_batched(pool, 0, 100)
            .await
            .expect("prune_old_events_batched");
        assert_eq!(deleted, 0, "days=0 must be a no-op (guard against 'delete all')");

        let remaining: (i64,) =
            sqlx::query_as("SELECT COUNT(*)::bigint FROM session_timeline")
                .fetch_one(pool)
                .await
                .expect("count remaining");
        assert_eq!(remaining.0, 50, "nothing deleted when days=0");
    })
    .await
    .expect("prune_batched_zero_days_is_noop exceeded 120s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prune_batched_rejects_non_positive_batch_size() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new().await.expect("spawn PG");
        let pool = harness.pool();

        let result = prune_old_events_batched(pool, 7, 0).await;
        assert!(
            result.is_err(),
            "batch_size=0 must error; got {:?}",
            result
        );

        let result = prune_old_events_batched(pool, 7, -1).await;
        assert!(
            result.is_err(),
            "batch_size=-1 must error; got {:?}",
            result
        );
    })
    .await
    .expect("prune_batched_rejects_non_positive_batch_size exceeded 120s");
}

/// RES-03 Open Question 4 — does batched DELETE on 15k rows complete in a
/// reasonable time without a `created_at` index? This is the explicit
/// benchmark requested by the plan's <output> section.
///
/// Run with: `cargo test -p opex-core --test integration_session_timeline_cleanup -- --ignored`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow — run explicitly to validate 15k-row batching path"]
async fn prune_batched_15k_rows_three_iterations() {
    timeout(Duration::from_secs(600), async {
        let harness = TestHarness::new().await.expect("spawn PG");
        let pool = harness.pool();

        // Seed 15_000 old + 500 fresh. Batch 5000 → 3 full iterations + termination.
        seed_events(pool, 15_000, 10).await;
        seed_events(pool, 500, 1).await;

        let started = std::time::Instant::now();
        let deleted = prune_old_events_batched(pool, 7, 5000)
            .await
            .expect("prune_old_events_batched");
        let elapsed = started.elapsed();
        eprintln!(
            "prune_old_events_batched(15_000 rows, batch=5000) took {:?}",
            elapsed
        );
        assert_eq!(deleted, 15_000, "15_000 old rows deleted");

        let remaining: (i64,) =
            sqlx::query_as("SELECT COUNT(*)::bigint FROM session_timeline")
                .fetch_one(pool)
                .await
                .expect("count remaining");
        assert_eq!(remaining.0, 500, "500 fresh preserved");
    })
    .await
    .expect("prune_batched_15k_rows_three_iterations exceeded 600s");
}
