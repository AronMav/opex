//! Phase 63 DATA-01 + DATA-05: Migration 022 indexes.
//!
//! Asserts:
//!   1. Migration 022 applies in <5s on 50k+50k fixture (via REINDEX proxy —
//!      we cannot time migration during TestHarness::new() without a custom
//!      variant; REINDEX has the same wall-clock characteristics as CREATE
//!      INDEX against an existing table).
//!   2. Concurrent reads during REINDEX never block for >500ms.
//!   3. EXPLAIN (FORMAT JSON) of the literal `WHERE read = FALSE` query
//!      names idx_notifications_unread and uses an Index/Bitmap Index Scan.
//!   4. Parameterised `WHERE read = $1` does NOT pick the partial index —
//!      regression guard against silent prepared-statement drift.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::{fixtures, TestHarness};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_022_indexes_exist_after_harness_spin_up() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();

        let sessions_idx: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM pg_indexes \
             WHERE indexname = 'idx_sessions_agent_user_channel_last_msg'",
        )
        .fetch_one(pool)
        .await
        .expect("query pg_indexes");
        assert_eq!(
            sessions_idx.0, 1,
            "migration 022 must create idx_sessions_agent_user_channel_last_msg"
        );

        let notif_idx: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM pg_indexes \
             WHERE indexname = 'idx_notifications_unread'",
        )
        .fetch_one(pool)
        .await
        .expect("query pg_indexes");
        assert_eq!(
            notif_idx.0, 1,
            "migration 022 must create idx_notifications_unread"
        );
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: seeds 50k+50k rows and REINDEXes — run explicitly"]
async fn reindex_completes_under_budget_on_50k_fixture() {
    timeout(Duration::from_secs(300), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();

        fixtures::seed_sessions(pool, 50_000)
            .await
            .expect("seed 50k sessions");
        fixtures::seed_notifications(pool, 50_000, 500)
            .await
            .expect("seed 50k notifications, 500 unread");

        // Proxy for migration wall-clock: REINDEX over already-seeded data.
        // Budget 5s comes from ROADMAP success criterion #1.
        let budget_ms: u64 = std::env::var("HYDECLAW_MIGRATION_BUDGET_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000);

        let start = Instant::now();
        sqlx::query("REINDEX INDEX idx_sessions_agent_user_channel_last_msg")
            .execute(pool)
            .await
            .expect("REINDEX sessions index");
        sqlx::query("REINDEX INDEX idx_notifications_unread")
            .execute(pool)
            .await
            .expect("REINDEX notifications index");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(budget_ms),
            "REINDEX wall-clock {elapsed:?} exceeded budget {budget_ms}ms \
             (override via HYDECLAW_MIGRATION_BUDGET_MS)"
        );
        eprintln!("reindex_completes_under_budget_on_50k_fixture: elapsed={elapsed:?}");
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: seeds 50k rows and runs concurrent REINDEX + SELECTs"]
async fn concurrent_read_never_blocks_long_during_reindex() {
    timeout(Duration::from_secs(300), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = Arc::new(harness.pool().clone());

        fixtures::seed_sessions(pool.as_ref(), 50_000)
            .await
            .expect("seed 50k sessions");

        // Spawn 10 concurrent readers.
        let mut handles = Vec::new();
        for _ in 0..10 {
            let p = Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                let start = Instant::now();
                let _count: (i64,) =
                    sqlx::query_as("SELECT COUNT(*)::bigint FROM sessions")
                        .fetch_one(p.as_ref())
                        .await
                        .expect("concurrent read");
                start.elapsed()
            }));
        }

        // REINDEX on the main task.
        sqlx::query("REINDEX INDEX idx_sessions_agent_user_channel_last_msg")
            .execute(pool.as_ref())
            .await
            .expect("REINDEX during concurrent reads");

        for h in handles {
            let elapsed = h.await.expect("reader task");
            assert!(
                elapsed < Duration::from_millis(500),
                "concurrent SELECT blocked for {elapsed:?} — expected <500ms \
                 because plain CREATE INDEX / REINDEX takes SHARE lock only"
            );
        }
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_index_used_by_literal_where_read_false() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();

        // Seed fixture: 10k total, 50 unread — low-selectivity unread
        // makes the partial index highly attractive to the planner.
        fixtures::seed_notifications(pool, 10_000, 50)
            .await
            .expect("seed notifications");
        sqlx::query("ANALYZE notifications")
            .execute(pool)
            .await
            .expect("ANALYZE");

        let explain: (Value,) = sqlx::query_as(
            "EXPLAIN (FORMAT JSON) \
             SELECT id, type, title, body, data, read, created_at \
             FROM notifications \
             WHERE read = FALSE \
             ORDER BY created_at DESC LIMIT 50",
        )
        .fetch_one(pool)
        .await
        .expect("EXPLAIN FORMAT JSON");

        let plan_root = explain
            .0
            .pointer("/0/Plan")
            .expect("EXPLAIN JSON has /0/Plan root");

        // Walk the plan tree (may be nested under Plans[] for LIMIT → Sort → IndexScan).
        let index_name = find_index_name_recursive(plan_root)
            .expect("plan tree must contain an Index Name somewhere");
        assert_eq!(
            index_name, "idx_notifications_unread",
            "expected partial index idx_notifications_unread to be used for \
             literal `WHERE read = FALSE`; got {index_name:?}. \
             Full plan: {:#}",
            explain.0
        );

        let node_type = find_index_scan_node_type_recursive(plan_root)
            .expect("plan must contain an Index Scan or Bitmap Index Scan node");
        assert!(
            node_type == "Index Scan" || node_type == "Bitmap Index Scan",
            "expected Index Scan or Bitmap Index Scan, got {node_type:?} — \
             partial-index contract broken. Full plan: {:#}",
            explain.0
        );
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(non_snake_case)]
async fn partial_index_NOT_used_by_parameterised_where_read_eq_dollar1() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();

        fixtures::seed_notifications(pool, 10_000, 50)
            .await
            .expect("seed notifications");
        sqlx::query("ANALYZE notifications")
            .execute(pool)
            .await
            .expect("ANALYZE");

        // DATA-05 regression guard: EXPLAIN with bound parameters under the
        // simple-protocol path that sqlx uses runs a *custom plan* — PG sees
        // the literal bind value at plan time, so it CAN still pick the
        // partial index. The real production risk is a prepared statement
        // that gets reused enough times to trigger PG's generic plan
        // (plan_cache_mode=force_generic_plan simulates this deterministically).
        //
        // Under a generic plan, the planner cannot prove `$1 = FALSE` at
        // plan time, and therefore MUST NOT select the partial index.
        // This pins the DATA-05 contract: our production queries use
        // literal `WHERE read = FALSE` precisely so this degradation never
        // occurs when PG starts generic-planning our prepared statement.
        //
        // We acquire a single connection so PREPARE/EXECUTE/SET share state.
        let mut conn = pool.acquire().await.expect("acquire conn");

        sqlx::query("SET plan_cache_mode = force_generic_plan")
            .execute(&mut *conn)
            .await
            .expect("force generic plan");

        sqlx::query(
            "PREPARE unread_q (bool) AS \
             SELECT id, type, title, body, data, read, created_at \
             FROM notifications \
             WHERE read = $1 \
             ORDER BY created_at DESC LIMIT 50",
        )
        .execute(&mut *conn)
        .await
        .expect("PREPARE");

        let explain: (Value,) = sqlx::query_as(
            "EXPLAIN (FORMAT JSON) EXECUTE unread_q(FALSE)",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("EXPLAIN EXECUTE generic plan");

        let plan_root = explain
            .0
            .pointer("/0/Plan")
            .expect("plan root");

        // Under generic plan, the planner cannot prove `$1 = FALSE` and
        // therefore MUST fall back to the full-table non-partial path
        // (either a Seq Scan or the non-partial notifications_read_created_at).
        let idx_hit = find_index_name_recursive(plan_root);
        assert_ne!(
            idx_hit.as_deref(),
            Some("idx_notifications_unread"),
            "parameterised `WHERE read = $1` under generic plan MUST NOT pick \
             the partial index idx_notifications_unread — it would mean PG \
             silently proved the bool predicate. This pins DATA-05: \
             production code MUST use literal `WHERE read = FALSE`. \
             Got plan: {:#}",
            explain.0
        );
    })
    .await
    .expect("timeout");
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Recursively search the EXPLAIN JSON plan tree for the first `Index Name`.
fn find_index_name_recursive(node: &Value) -> Option<String> {
    if let Some(name) = node.pointer("/Index Name").and_then(|v| v.as_str()) {
        return Some(name.to_string());
    }
    if let Some(plans) = node.pointer("/Plans").and_then(|v| v.as_array()) {
        for child in plans {
            if let Some(n) = find_index_name_recursive(child) {
                return Some(n);
            }
        }
    }
    None
}

/// Recursively find the first `Node Type` that is an index scan variant.
fn find_index_scan_node_type_recursive(node: &Value) -> Option<String> {
    if let Some(nt) = node.pointer("/Node Type").and_then(|v| v.as_str())
        && (nt == "Index Scan" || nt == "Bitmap Index Scan")
    {
        return Some(nt.to_string());
    }
    if let Some(plans) = node.pointer("/Plans").and_then(|v| v.as_array()) {
        for child in plans {
            if let Some(nt) = find_index_scan_node_type_recursive(child) {
                return Some(nt);
            }
        }
    }
    None
}
