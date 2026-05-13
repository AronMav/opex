//! Integration tests for `set_session_run_status` FSM.
//!
//! The conditional-on-running transition is now handled by
//! `cleanup_session_terminated`, which has its own tests in
//! `hydeclaw-db/src/sessions.rs`. This file only retains the FSM
//! regression test for `set_session_run_status` (which restricts
//! transitions FROM 'running' or NULL).
//!
//! Gated to Linux x86_64 because testcontainers requires Docker
//! (matches the pattern used by `integration_aborted_usage.rs`).

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hydeclaw_core::db::sessions::set_session_run_status;
use sqlx::PgPool;
use uuid::Uuid;

async fn seed_session(pool: &PgPool, session_id: Uuid, initial_status: &str) {
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel, title, started_at, last_message_at, run_status) \
         VALUES ($1, 'Arty', 'test-user', 'test', 'status-transition-test', NOW(), NOW(), $2) \
         ON CONFLICT (id) DO UPDATE SET run_status = EXCLUDED.run_status",
    )
    .bind(session_id)
    .bind(initial_status)
    .execute(pool)
    .await
    .expect("seed session row");
}

async fn current_status(pool: &PgPool, session_id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .expect("read run_status")
}

#[sqlx::test(migrations = "../../migrations")]
async fn set_session_run_status_only_advances_from_running_or_null(pool: PgPool) {
    // Regression guard for the SQL-level FSM: `set_session_run_status` only
    // permits transitions FROM `'running'` or NULL. Terminal→terminal jumps
    // (`interrupted → failed`, `failed → done`, etc.) are blocked at the
    // WHERE clause and silently no-op (rows_affected = 0). The previous
    // `IS DISTINCT FROM 'done'` predicate let those terminal jumps through
    // — see audit 2026-05-08, fix(group-c).
    let sid = Uuid::new_v4();
    seed_session(&pool, sid, "running").await;

    // running → interrupted is allowed.
    set_session_run_status(&pool, sid, "interrupted")
        .await
        .expect("interrupted");
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("interrupted"));

    // interrupted → failed must NOT advance: the helper documents
    // "Only allows transitions from 'running' or NULL".
    set_session_run_status(&pool, sid, "failed")
        .await
        .expect("attempted overwrite");
    assert_eq!(
        current_status(&pool, sid).await.as_deref(),
        Some("interrupted"),
        "terminal→terminal transition must be blocked",
    );

    // done is also a terminal that the helper must refuse to overwrite,
    // even from another terminal state.
    set_session_run_status(&pool, sid, "done")
        .await
        .expect("attempted overwrite");
    assert_eq!(
        current_status(&pool, sid).await.as_deref(),
        Some("interrupted"),
        "terminal→done must also be blocked",
    );

    // From a fresh `running` row we may transition to `done`.
    let sid2 = Uuid::new_v4();
    seed_session(&pool, sid2, "running").await;
    set_session_run_status(&pool, sid2, "done")
        .await
        .expect("done");
    assert_eq!(current_status(&pool, sid2).await.as_deref(), Some("done"));

    // And once `done`, no further transition is permitted (this was already
    // covered by `IS DISTINCT FROM 'done'`; preserved here as belt-and-
    // suspenders).
    set_session_run_status(&pool, sid2, "running")
        .await
        .expect("attempt overwrite done");
    assert_eq!(current_status(&pool, sid2).await.as_deref(), Some("done"));
}
