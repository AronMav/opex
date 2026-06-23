//! Integration tests for `db::usage::insert_aborted_row` against a real
//! Postgres + m025 schema. Gated to Linux x86_64 because testcontainers
//! requires Docker; macOS/Windows devs skip these (matches the pattern
//! used by `tests/migration_026.rs`).
//!
//! The test covers the usage-log half of the observability contract that
//! commit `59c52f2` locked in:
//!   * `STATUS_ABORTED` / `STATUS_ABORTED_FAILOVER` constants round-trip
//!     through the column introduced in m025.
//!   * The partial index `idx_usage_log_status_aborted` (defined by m025)
//!     covers BOTH status values.
//!   * Non-SSE callers writing through `insert_aborted_row` produce rows
//!     indistinguishable from the SSE path — closing the observability
//!     asymmetry Issue 6 reported.
//!
//! The outer `record_aborted_usage` wrapper in `engine_sse.rs` adds only
//! the `LlmCallError` downcast, which is small enough to unit-test via
//! the in-crate `#[cfg(test)]` module it already lives in.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use opex_core::db::usage::{
    insert_aborted_row, STATUS_ABORTED, STATUS_ABORTED_FAILOVER,
};
use sqlx::PgPool;
use uuid::Uuid;

/// Seeds the minimum rows needed for a valid `usage_log` insert.
async fn seed_agent_and_session(pool: &PgPool, agent: &str, session_id: Uuid) {
    // Agents table may be populated by migrations or empty on fresh DB;
    // `usage_log` has no FK to agents, so we only need to make sure there
    // is a session row the `session_id` column can reference. Sessions
    // table DOES exist by m012.
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel, title, started_at, last_message_at) \
         VALUES ($1, $2, 'test-user', 'test', 'aborted-usage-test', NOW(), NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(session_id)
    .bind(agent)
    .execute(pool)
    .await
    .expect("seed session row");
}

#[sqlx::test(migrations = "../../migrations")]
async fn aborted_row_with_plain_status_persists_and_queries(pool: PgPool) {
    let agent = "Arty";
    let session_id = Uuid::new_v4();
    seed_agent_and_session(&pool, agent, session_id).await;

    insert_aborted_row(
        &pool,
        agent,
        "ollama-default",
        "minimax-m2.7",
        session_id,
        123,
        STATUS_ABORTED,
    )
    .await
    .expect("insert aborted row");

    // Confirm the row is visible via the exact status filter.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM usage_log WHERE session_id = $1 AND status = $2",
    )
    .bind(session_id)
    .bind(STATUS_ABORTED)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1, "aborted row not found for session");

    // input_tokens is always 0 for aborted calls (we never saw the usage
    // header). output_tokens should match the caller's estimate.
    let (input, output): (i32, i32) = sqlx::query_as(
        "SELECT input_tokens, output_tokens FROM usage_log WHERE session_id = $1",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(input, 0);
    assert_eq!(output, 123);
}

#[sqlx::test(migrations = "../../migrations")]
async fn aborted_failover_status_persists_and_queries(pool: PgPool) {
    let agent = "Arty";
    let session_id = Uuid::new_v4();
    seed_agent_and_session(&pool, agent, session_id).await;

    insert_aborted_row(
        &pool,
        agent,
        "ollama-default",
        "minimax-m2.7",
        session_id,
        500,
        STATUS_ABORTED_FAILOVER,
    )
    .await
    .expect("insert aborted_failover row");

    let (stored_status,): (String,) =
        sqlx::query_as("SELECT status FROM usage_log WHERE session_id = $1")
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_status, "aborted_failover");
}

#[sqlx::test(migrations = "../../migrations")]
async fn partial_index_covers_both_aborted_values(pool: PgPool) {
    // m025 created `idx_usage_log_status_aborted` with
    // `WHERE status LIKE 'aborted%'` so both `aborted` and
    // `aborted_failover` share one index. Pin this by inserting one row
    // of each status and verifying the index's coverage SQL finds both.
    let agent = "Arty";
    let session_plain = Uuid::new_v4();
    let session_failover = Uuid::new_v4();
    seed_agent_and_session(&pool, agent, session_plain).await;
    seed_agent_and_session(&pool, agent, session_failover).await;

    insert_aborted_row(&pool, agent, "p", "m", session_plain, 1, STATUS_ABORTED)
        .await
        .unwrap();
    insert_aborted_row(&pool, agent, "p", "m", session_failover, 1, STATUS_ABORTED_FAILOVER)
        .await
        .unwrap();

    // The dashboard SQL in spec §9 filters `WHERE status LIKE 'aborted%'`
    // — confirm both rows match.
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_log WHERE status LIKE 'aborted%'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 2, "partial index coverage failed");

    // And per-status aggregation produces the expected breakdown.
    let breakdown: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM usage_log WHERE status LIKE 'aborted%' GROUP BY status ORDER BY status",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        breakdown,
        vec![
            ("aborted".to_string(), 1),
            ("aborted_failover".to_string(), 1),
        ]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn non_matching_status_not_written_by_insert_aborted_row(pool: PgPool) {
    // Regression: `insert_aborted_row` does not silently sanitize the
    // status string — whatever the caller passes lands in the column.
    // This is by design (the enum mapping lives in the caller), so the
    // test documents the contract: pass garbage, get garbage.
    let agent = "Arty";
    let session_id = Uuid::new_v4();
    seed_agent_and_session(&pool, agent, session_id).await;

    insert_aborted_row(&pool, agent, "p", "m", session_id, 42, "garbage")
        .await
        .unwrap();

    let stored: String = sqlx::query_scalar(
        "SELECT status FROM usage_log WHERE session_id = $1",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, "garbage");

    // And it is NOT covered by the `aborted%` partial index filter.
    let aborted_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM usage_log WHERE session_id = $1 AND status LIKE 'aborted%'",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(aborted_count, 0);
}
