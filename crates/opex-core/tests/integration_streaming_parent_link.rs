//! Integration test for Bug 2: `upsert_streaming_message` must thread
//! `parent_message_id` to the most-recent user message for the session.
//!
//! Pre-fix: the streaming assistant placeholder row is INSERTed with
//! `parent_message_id = NULL`, creating a second orphan root that
//! `resolveActivePath` (UI) can pick as `roots[0]` and shadow the real
//! conversation tree.
//!
//! Post-fix: the INSERT uses a correlated subquery
//! `(SELECT id FROM messages WHERE session_id = $N AND role = 'user'
//!   ORDER BY created_at DESC LIMIT 1)` so every streaming row is anchored
//! to the preceding user message.
//!
//! Gated to Linux x86_64 because testcontainers requires Docker (matches
//! the pattern used by `integration_session_run_status.rs`).

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use opex_core::db::sessions::upsert_streaming_message;
use sqlx::PgPool;
use uuid::Uuid;

/// Seed a minimal session + user message so the streaming-row INSERT has a
/// parent candidate to latch onto.
async fn seed_session_with_user(pool: &PgPool) -> (Uuid, Uuid) {
    let session_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel, title, started_at, last_message_at, run_status) \
         VALUES ($1, 'Arty', 'test-user', 'web', 'streaming-parent-link', NOW(), NOW(), 'running')",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .expect("seed session row");
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, agent_id, status, created_at) \
         VALUES ($1, $2, 'user', 'hi', NULL, 'complete', NOW())",
    )
    .bind(user_id)
    .bind(session_id)
    .execute(pool)
    .await
    .expect("seed user row");
    (session_id, user_id)
}

#[sqlx::test(migrations = "../../migrations")]
async fn upsert_streaming_message_sets_parent_to_latest_user(pool: PgPool) {
    let (session_id, user_id) = seed_session_with_user(&pool).await;

    let streaming_id = Uuid::new_v4();
    upsert_streaming_message(&pool, streaming_id, session_id, "Arty", "", None)
        .await
        .expect("upsert streaming row");

    let parent: Option<Uuid> = sqlx::query_scalar(
        "SELECT parent_message_id FROM messages WHERE id = $1",
    )
    .bind(streaming_id)
    .fetch_one(&pool)
    .await
    .expect("read parent_message_id");

    assert_eq!(
        parent,
        Some(user_id),
        "streaming row parent_message_id must point at the latest user message (got {:?}, expected {})",
        parent,
        user_id
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn upsert_streaming_message_update_preserves_parent_across_appends(pool: PgPool) {
    // Contract: parent_message_id is set ONCE on INSERT and never touched by
    // ON CONFLICT DO UPDATE. This guards against a naive re-SELECT of the
    // latest user row (which could race and flip the parent mid-stream).
    let (session_id, user_id) = seed_session_with_user(&pool).await;

    let streaming_id = Uuid::new_v4();
    upsert_streaming_message(&pool, streaming_id, session_id, "Arty", "", None)
        .await
        .expect("first upsert");

    // Insert a NEWER user row to pollute "latest user" — if the UPDATE path
    // re-evaluated the subquery it would flip the parent to user2_id below.
    let user2_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, agent_id, status, created_at) \
         VALUES ($1, $2, 'user', 'later', NULL, 'complete', NOW() + INTERVAL '10 seconds')",
    )
    .bind(user2_id)
    .bind(session_id)
    .execute(&pool)
    .await
    .expect("seed later user row");

    // Second upsert — goes through ON CONFLICT DO UPDATE branch.
    upsert_streaming_message(&pool, streaming_id, session_id, "Arty", "partial text", None)
        .await
        .expect("second upsert");

    let parent: Option<Uuid> = sqlx::query_scalar(
        "SELECT parent_message_id FROM messages WHERE id = $1",
    )
    .bind(streaming_id)
    .fetch_one(&pool)
    .await
    .expect("read parent_message_id");

    assert_eq!(
        parent,
        Some(user_id),
        "parent_message_id must be pinned at first-INSERT user (got {:?}, expected {})",
        parent,
        user_id
    );
    // And the content was refreshed.
    let content: String = sqlx::query_scalar("SELECT content FROM messages WHERE id = $1")
        .bind(streaming_id)
        .fetch_one(&pool)
        .await
        .expect("read content");
    assert_eq!(content, "partial text");
}
