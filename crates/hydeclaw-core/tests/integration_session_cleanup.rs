//! Session cleanup integration tests — verify I1 (single termination path)
//! holds across watchdog/finalize race scenarios.
//!
//! Gated to Linux x86_64 to match the pattern in `integration_session_run_status.rs`
//! and `integration_aborted_usage.rs` (sqlx::test + Docker testcontainers).

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use sqlx::PgPool;
use hydeclaw_core::db::sessions::{
    create_new_session, set_session_run_status, cleanup_session_terminated,
    find_stale_running_sessions_per_agent,
};
use hydeclaw_core::db::session_timeline::log_event_tx;
use std::collections::HashMap;

/// T2.1: watchdog must never overwrite a `done` status (B2 regression).
#[sqlx::test(migrations = "../../migrations")]
async fn watchdog_cannot_overwrite_done(pool: PgPool) {
    let s = create_new_session(&pool, "a", "u", "web").await.unwrap();
    set_session_run_status(&pool, s, "running").await.unwrap();

    // Simulate the race: finalize sets 'done' first.
    set_session_run_status(&pool, s, "done").await.unwrap();
    // Watchdog tries to cleanup as 'timeout' — must be no-op.
    let claimed = cleanup_session_terminated(&pool, s, "timeout", "race_test")
        .await.unwrap();
    assert!(!claimed, "cleanup must return false when session not 'running'");

    let status: String = sqlx::query_scalar(
        "SELECT run_status FROM sessions WHERE id = $1"
    ).bind(s).fetch_one(&pool).await.unwrap();
    assert_eq!(status, "done", "done status preserved (B2 fix)");
}

/// T2.2: Timeline-event heartbeat prevents false-positive timeout during long
/// tool/LLM calls (B3 fix).
#[sqlx::test(migrations = "../../migrations")]
async fn timeline_event_heartbeat_prevents_false_positive(pool: PgPool) {
    let s = create_new_session(&pool, "a", "u", "web").await.unwrap();
    sqlx::query(
        "UPDATE sessions SET run_status = 'running', activity_at = NOW() - INTERVAL '90 seconds'
         WHERE id = $1"
    ).bind(s).execute(&pool).await.unwrap();

    // Imitate a tool_start during a long LLM call — heartbeat must refresh activity_at.
    let mut tx = pool.begin().await.unwrap();
    log_event_tx(&mut tx, s, "tool_start", None).await.unwrap();
    tx.commit().await.unwrap();

    // Watchdog query with 60s threshold should NOT find this session.
    let mut map = HashMap::new();
    map.insert("a".to_string(), 60i64);
    let stale = find_stale_running_sessions_per_agent(&pool, &map, 60).await.unwrap();
    assert!(stale.iter().all(|t| t.0 != s),
        "session must not be stale after timeline heartbeat (B3 fix)");
}

/// T2.3: watchdog and startup-cleanup produce equivalent terminal state for
/// the same scenario (I1 — single cleanup path).
#[sqlx::test(migrations = "../../migrations")]
async fn watchdog_and_startup_produce_equivalent_terminal_state(pool: PgPool) {
    // Sessions A and B: identical setup. A goes through cleanup() with
    // target=timeout (watchdog scenario), B with target=interrupted
    // (startup-cleanup scenario). Both must preserve partial-text and
    // write timeline events.
    let s_a = create_new_session(&pool, "a", "u", "web").await.unwrap();
    let s_b = create_new_session(&pool, "a", "u", "web").await.unwrap();
    for sid in [s_a, s_b] {
        set_session_run_status(&pool, sid, "running").await.unwrap();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, status)
             VALUES ($1, 'assistant', 'partial-X', 'streaming')"
        ).bind(sid).execute(&pool).await.unwrap();
    }

    cleanup_session_terminated(&pool, s_a, "timeout", "watchdog").await.unwrap();
    cleanup_session_terminated(&pool, s_b, "interrupted", "crash_recovery").await.unwrap();

    let row_a: (String, String) = sqlx::query_as(
        "SELECT
            (SELECT run_status FROM sessions WHERE id = $1),
            (SELECT content FROM messages WHERE session_id = $1)"
    ).bind(s_a).fetch_one(&pool).await.unwrap();
    let row_b: (String, String) = sqlx::query_as(
        "SELECT
            (SELECT run_status FROM sessions WHERE id = $1),
            (SELECT content FROM messages WHERE session_id = $1)"
    ).bind(s_b).fetch_one(&pool).await.unwrap();

    assert_eq!(row_a.0, "timeout");
    assert_eq!(row_b.0, "interrupted");
    assert_eq!(row_a.1, "partial-X", "watchdog preserves partial");
    assert_eq!(row_b.1, "partial-X", "startup-cleanup preserves partial");
}
