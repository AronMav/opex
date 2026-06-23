//! Phase 63 DATA-03: chunked batch INSERT for insert_synthetic_tool_results.
//!
//! Round-trip count assertion via pg_stat_statements. Requires
//! TestHarness::new_with_pg_stat_statements() from Plan 01.
//!
//! Chunk size formula (CONTEXT.md correction #5):
//!   MAX_PARAMS_PER_QUERY = 32767  (half of PG wire protocol 65535)
//!   BIND_COUNT_PER_ROW   = 4      (id, session_id, content, tool_call_id)
//!   chunk_size           = 32767 / 4 = 8191 rows per round trip
//!
//! Literals `'tool'`, `NOW()`, `'complete'` in the row template do NOT count
//! toward the bind budget — correction #5 pins this semantic.

mod support;

use std::time::Duration;

use opex_core::db::sessions::insert_synthetic_tool_results;
use serde_json::json;
use sqlx::PgPool;
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

/// Must match the module-level constant BIND_COUNT_PER_ROW in src/db/sessions.rs.
/// If Task 2 changes this, update here too.
const TEST_CHUNK_SIZE: usize = 32767 / 4; // = 8191

/// Seed one session + one assistant message containing `n` fresh tool_call_ids.
/// No corresponding tool rows exist → all `n` ids are "missing" and will be
/// inserted by insert_synthetic_tool_results.
async fn seed_assistant_with_tool_calls(pool: &PgPool, n: usize) -> Uuid {
    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel) \
         VALUES ($1, 'batch-test-agent', 'batch-user', 'web')",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .expect("insert session");

    let tool_calls: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            json!({
                "id": format!("call-{}-{}", session_id, i),
                "type": "function",
                "function": {"name": "x", "arguments": "{}"}
            })
        })
        .collect();

    sqlx::query(
        "INSERT INTO messages (id, session_id, agent_id, role, content, tool_calls) \
         VALUES ($1, $2, 'batch-test-agent', 'assistant', '', $3::jsonb)",
    )
    .bind(Uuid::new_v4())
    .bind(session_id)
    .bind(serde_json::Value::Array(tool_calls))
    .execute(pool)
    .await
    .expect("insert assistant message");

    session_id
}

/// Query pg_stat_statements for the synthetic-INSERT call count.
/// Matches on the LIKE pattern of the known INSERT query prefix.
async fn call_count_for_synthetic_insert(pool: &PgPool) -> i64 {
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT SUM(calls)::bigint FROM pg_stat_statements \
         WHERE query LIKE 'INSERT INTO messages%tool_call_id%'",
    )
    .fetch_optional(pool)
    .await
    .expect("pg_stat_statements query");
    row.and_then(|(c,)| c).unwrap_or(0)
}

async fn reset_pg_stat(pool: &PgPool) {
    // pg_stat_statements_reset() returns void — use execute(), not query_as
    // (tuple-decode on void would fail at runtime with a decode error).
    sqlx::query("SELECT pg_stat_statements_reset()")
        .execute(pool)
        .await
        .expect("pg_stat_statements_reset()");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn small_batch_one_round_trip() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new_with_pg_stat_statements()
            .await
            .expect("pg_stat_statements harness");
        let pool = harness.pool();

        let session_id = seed_assistant_with_tool_calls(pool, 10).await;
        reset_pg_stat(pool).await;

        let inserted = insert_synthetic_tool_results(pool, session_id)
            .await
            .expect("insert_synthetic_tool_results");
        assert_eq!(inserted, 10, "must insert 10 synthetic rows");

        let calls = call_count_for_synthetic_insert(pool).await;
        assert_eq!(
            calls, 1,
            "10 rows must be written in a single SQL round-trip (one chunk); \
             got {calls} pg_stat_statements.calls for the INSERT"
        );
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_chunk_rounds_up_to_two_calls() {
    timeout(Duration::from_secs(180), async {
        let harness = TestHarness::new_with_pg_stat_statements()
            .await
            .expect("harness");
        let pool = harness.pool();

        // TEST_CHUNK_SIZE + 1 = 8192 → expect 2 chunks.
        let n = TEST_CHUNK_SIZE + 1;
        let session_id = seed_assistant_with_tool_calls(pool, n).await;
        reset_pg_stat(pool).await;

        let inserted = insert_synthetic_tool_results(pool, session_id)
            .await
            .expect("insert");
        assert_eq!(inserted, n);

        let calls = call_count_for_synthetic_insert(pool).await;
        assert_eq!(
            calls, 2,
            "{n} rows must be written in exactly 2 chunks \
             (chunk_size={TEST_CHUNK_SIZE}); got {calls} calls"
        );
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: seeds 16382 tool_calls and runs 2 chunks"]
async fn multi_chunk_exact_boundary() {
    timeout(Duration::from_secs(300), async {
        let harness = TestHarness::new_with_pg_stat_statements()
            .await
            .expect("harness");
        let pool = harness.pool();

        let n = TEST_CHUNK_SIZE * 2;
        let session_id = seed_assistant_with_tool_calls(pool, n).await;
        reset_pg_stat(pool).await;

        let inserted = insert_synthetic_tool_results(pool, session_id)
            .await
            .expect("insert");
        assert_eq!(inserted, n);

        let calls = call_count_for_synthetic_insert(pool).await;
        assert_eq!(calls, 2, "exactly 2 chunks at boundary");
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_missing_zero_calls() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new_with_pg_stat_statements()
            .await
            .expect("harness");
        let pool = harness.pool();

        // Seed 3 tool_call_ids AND matching tool rows — nothing missing.
        let session_id = seed_assistant_with_tool_calls(pool, 3).await;
        for i in 0..3 {
            sqlx::query(
                "INSERT INTO messages (id, session_id, agent_id, role, content, tool_call_id) \
                 VALUES ($1, $2, 'batch-test-agent', 'tool', 'existing-result', $3)",
            )
            .bind(Uuid::new_v4())
            .bind(session_id)
            .bind(format!("call-{}-{}", session_id, i))
            .execute(pool)
            .await
            .expect("seed tool row");
        }
        reset_pg_stat(pool).await;

        let inserted = insert_synthetic_tool_results(pool, session_id)
            .await
            .expect("call");
        assert_eq!(
            inserted, 0,
            "nothing to insert when all call_ids already have tool rows"
        );

        let calls = call_count_for_synthetic_insert(pool).await;
        assert_eq!(calls, 0, "zero INSERTs when nothing is missing");
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rowcount_correct_after_batching() {
    timeout(Duration::from_secs(180), async {
        let harness = TestHarness::new_with_pg_stat_statements()
            .await
            .expect("harness");
        let pool = harness.pool();

        let session_id = seed_assistant_with_tool_calls(pool, 100).await;

        let inserted = insert_synthetic_tool_results(pool, session_id)
            .await
            .expect("call");
        assert_eq!(inserted, 100);

        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM messages WHERE session_id = $1 AND role = 'tool'",
        )
        .bind(session_id)
        .fetch_one(pool)
        .await
        .expect("count");
        assert_eq!(count.0, 100, "all 100 synthetic rows must be persisted");
    })
    .await
    .expect("timeout");
}
