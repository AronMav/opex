//! Integration tests for `mirror_to_session` — the delivery-mirror path used
//! by the cron scheduler to write a passive context record into an existing
//! DM session without triggering the `update_session_last_message` trigger.
//!
//! All tests run against a real PostgreSQL container (via `#[sqlx::test]`).
//! Gated to Linux x86_64 because testcontainers requires Docker.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hydeclaw_core::db::sessions::mirror_to_session;
use sqlx::PgPool;
use uuid::Uuid;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Insert a minimal session row and return its UUID.
async fn insert_dm_session(pool: &PgPool, agent_id: &str, user_id: &str, channel: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(agent_id)
    .bind(user_id)
    .bind(channel)
    .execute(pool)
    .await
    .expect("insert session");
    id
}

/// Read `last_message_at` for the given session.
async fn last_message_at(pool: &PgPool, session_id: Uuid) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar("SELECT last_message_at FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .expect("read last_message_at")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Happy path: a DM session exists → `mirror_to_session` inserts a record and
/// returns `Ok(true)`.
#[sqlx::test(migrations = "../../migrations")]
async fn mirror_inserts_when_dm_session_exists(pool: PgPool) {
    let agent = format!("mirror-test-{}", Uuid::new_v4());
    let session_id = insert_dm_session(&pool, &agent, "12345", "telegram").await;

    let result = mirror_to_session(&pool, &agent, "telegram", "12345", "hello from cron").await;
    assert!(result.unwrap(), "should return true when DM session exists");

    let (role, content, is_mirror): (String, String, bool) = sqlx::query_as(
        "SELECT role, content, is_mirror \
         FROM messages \
         WHERE session_id = $1 AND is_mirror = true",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .expect("mirror message not found");

    assert_eq!(role, "assistant");
    assert_eq!(content, "hello from cron");
    assert!(is_mirror);
}

/// No matching session → `mirror_to_session` returns `Ok(false)` and inserts
/// nothing.
#[sqlx::test(migrations = "../../migrations")]
async fn mirror_returns_false_when_no_matching_session(pool: PgPool) {
    let result =
        mirror_to_session(&pool, "nonexistent-agent", "telegram", "99999", "nobody home").await;
    assert!(!result.unwrap(), "should return false when no matching session exists");

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE is_mirror = true AND agent_id = $1",
    )
    .bind("nonexistent-agent")
    .fetch_one(&pool)
    .await
    .expect("count query");
    assert_eq!(count, 0, "no messages should have been inserted");
}

/// Per-chat group sessions (`user_id = '*'`) must be excluded — they are not
/// DM sessions and should never receive mirror records.
#[sqlx::test(migrations = "../../migrations")]
async fn mirror_skips_per_chat_group_sessions(pool: PgPool) {
    let agent = format!("mirror-group-{}", Uuid::new_v4());
    insert_dm_session(&pool, &agent, "*", "telegram").await;

    let result = mirror_to_session(&pool, &agent, "telegram", "*", "group text").await;
    assert!(!result.unwrap(), "per-chat group sessions should be skipped");
}

/// The DB trigger `update_session_last_message` must NOT bump
/// `sessions.last_message_at` when `is_mirror = true`.
#[sqlx::test(migrations = "../../migrations")]
async fn trigger_does_not_bump_last_message_at_for_mirror(pool: PgPool) {
    let agent = format!("mirror-trigger-{}", Uuid::new_v4());
    let session_id = insert_dm_session(&pool, &agent, "55555", "telegram").await;

    let before = last_message_at(&pool, session_id).await;

    let mirrored = mirror_to_session(&pool, &agent, "telegram", "55555", "cron delivery")
        .await
        .expect("mirror_to_session");
    assert!(mirrored, "mirror_to_session must return true when session exists");

    let after = last_message_at(&pool, session_id).await;
    assert_eq!(
        before, after,
        "trigger must not bump last_message_at for mirror records"
    );
}

/// When two DM sessions exist for the same agent+channel+user, the mirror
/// record must be inserted into the most-recently-started session.
#[sqlx::test(migrations = "../../migrations")]
async fn mirror_uses_most_recent_session(pool: PgPool) {
    let agent = format!("mirror-order-{}", Uuid::new_v4());

    // Insert older session first.
    let older_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel, started_at) \
         VALUES ($1, $2, '77777', 'telegram', NOW() - INTERVAL '1 hour')",
    )
    .bind(older_id)
    .bind(&agent)
    .execute(&pool)
    .await
    .expect("insert older session");

    // Insert newer session.
    let newer_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel, started_at) \
         VALUES ($1, $2, '77777', 'telegram', NOW())",
    )
    .bind(newer_id)
    .bind(&agent)
    .execute(&pool)
    .await
    .expect("insert newer session");

    let result = mirror_to_session(&pool, &agent, "telegram", "77777", "latest session check").await;
    assert!(result.unwrap(), "should return true");

    // The mirror row must be in the newer session.
    let count_newer: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE session_id = $1 AND is_mirror = true")
            .bind(newer_id)
            .fetch_one(&pool)
            .await
            .expect("count newer");

    let count_older: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE session_id = $1 AND is_mirror = true")
            .bind(older_id)
            .fetch_one(&pool)
            .await
            .expect("count older");

    assert_eq!(count_newer, 1, "mirror must land in the newest session");
    assert_eq!(count_older, 0, "older session must not receive the mirror");
}

/// Verify the `is_mirror` field is readable from the messages table for the
/// inserted record.
#[sqlx::test(migrations = "../../migrations")]
async fn mirror_is_mirror_field_in_messages(pool: PgPool) {
    let agent = format!("mirror-field-{}", Uuid::new_v4());
    let session_id = insert_dm_session(&pool, &agent, "33333", "telegram").await;

    mirror_to_session(&pool, &agent, "telegram", "33333", "field check")
        .await
        .expect("mirror_to_session");

    let is_mirror: bool =
        sqlx::query_scalar("SELECT is_mirror FROM messages WHERE session_id = $1 AND is_mirror = true")
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .expect("fetch is_mirror");

    assert!(is_mirror, "is_mirror must be true for the inserted record");
}
