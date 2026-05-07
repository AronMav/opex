//! Integration tests for `mark_session_run_status_if_running` — the
//! conditional status transition used on the cancel-grace path to
//! prevent the `SessionLifecycleGuard`'s `'failed'` fallback from
//! overwriting an earlier `'interrupted'` write.
//!
//! The invariant under test: a session already in a terminal state
//! (`'done'`, `'failed'`, `'interrupted'`, `'timeout'`, `'cancelled'`)
//! cannot transition to a new status via this helper. Only
//! `'running'` sessions can.
//!
//! Gated to Linux x86_64 because testcontainers requires Docker
//! (matches the pattern used by `integration_aborted_usage.rs`).

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hydeclaw_core::db::sessions::{
    mark_session_run_status_if_running, set_session_run_status,
};
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
async fn transitions_running_to_interrupted(pool: PgPool) {
    let sid = Uuid::new_v4();
    seed_session(&pool, sid, "running").await;

    let affected = mark_session_run_status_if_running(&pool, sid, "interrupted")
        .await
        .expect("update query");
    assert_eq!(affected, 1, "expected 1 row updated on running → interrupted");
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("interrupted"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn transitions_running_to_failed(pool: PgPool) {
    // The Drop guard path.
    let sid = Uuid::new_v4();
    seed_session(&pool, sid, "running").await;

    let affected = mark_session_run_status_if_running(&pool, sid, "failed")
        .await
        .expect("update query");
    assert_eq!(affected, 1);
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("failed"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn does_not_overwrite_done(pool: PgPool) {
    let sid = Uuid::new_v4();
    seed_session(&pool, sid, "done").await;

    let affected = mark_session_run_status_if_running(&pool, sid, "failed")
        .await
        .expect("update query");
    assert_eq!(affected, 0, "must not overwrite done with failed");
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("done"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn does_not_overwrite_interrupted_with_failed(pool: PgPool) {
    // This is the critical race the helper prevents: the chat handler
    // writes `'interrupted'` on grace-exceeded, then the engine task is
    // hard-aborted, the guard drops, and its Drop impl tries to write
    // `'failed'`. That write MUST be a no-op.
    let sid = Uuid::new_v4();
    seed_session(&pool, sid, "running").await;

    // Handler writes interrupted first.
    mark_session_run_status_if_running(&pool, sid, "interrupted")
        .await
        .expect("first update");
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("interrupted"));

    // Guard drop then attempts failed — must be a no-op.
    let affected = mark_session_run_status_if_running(&pool, sid, "failed")
        .await
        .expect("second update");
    assert_eq!(affected, 0, "guard drop must not overwrite interrupted");
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("interrupted"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn does_not_overwrite_failed(pool: PgPool) {
    let sid = Uuid::new_v4();
    seed_session(&pool, sid, "failed").await;

    let affected = mark_session_run_status_if_running(&pool, sid, "interrupted")
        .await
        .expect("update query");
    assert_eq!(affected, 0);
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("failed"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn is_idempotent_for_already_terminal_sessions(pool: PgPool) {
    // Calling the helper twice on an already-terminal session is safe.
    let sid = Uuid::new_v4();
    seed_session(&pool, sid, "interrupted").await;

    for _ in 0..3 {
        let affected = mark_session_run_status_if_running(&pool, sid, "failed")
            .await
            .expect("update query");
        assert_eq!(affected, 0);
    }
    assert_eq!(current_status(&pool, sid).await.as_deref(), Some("interrupted"));
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
