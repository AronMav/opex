//! Integration: /api/watchdog/agent-activity endpoint.
//!
//! Tests verify the two DB queries the handler runs:
//!   1. `COALESCE(GREATEST(MAX(activity_at), MAX(last_message_at)), MAX(activity_at), MAX(last_message_at))`
//!      — latest activity across all sessions, with NULL fallback.
//!   2. `MAX(started_at) WHERE channel = 'heartbeat'` — last heartbeat session start.
//!
//! These cover the most likely runtime bugs (wrong column name, wrong table, NULL
//! handling) without requiring a full router harness.

use chrono::Utc;
use sqlx::PgPool;

/// T1: latest-activity query returns Some for an agent with a seeded session.
#[sqlx::test(migrations = "../../migrations")]
async fn latest_activity_query_returns_some_for_seeded_session(pool: PgPool) {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO sessions \
         (id, agent_id, user_id, channel, started_at, last_message_at, activity_at, run_status) \
         VALUES (gen_random_uuid(), 'TestAgent', 'u', 'web', $1, $1, $1, 'done')",
    )
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert regular session");

    let result: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT COALESCE( \
             GREATEST(MAX(activity_at), MAX(last_message_at)), \
             MAX(activity_at), \
             MAX(last_message_at)) \
         FROM sessions WHERE agent_id = $1",
    )
    .bind("TestAgent")
    .fetch_one(&pool)
    .await
    .expect("query");

    assert!(
        result.is_some(),
        "latest-activity query must return Some for a seeded session"
    );
}

/// T2: latest-activity query returns None for an agent with no sessions.
#[sqlx::test(migrations = "../../migrations")]
async fn latest_activity_query_returns_none_for_unknown_agent(pool: PgPool) {
    let result: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT COALESCE( \
             GREATEST(MAX(activity_at), MAX(last_message_at)), \
             MAX(activity_at), \
             MAX(last_message_at)) \
         FROM sessions WHERE agent_id = $1",
    )
    .bind("NoSuchAgent")
    .fetch_one(&pool)
    .await
    .expect("query");

    assert!(
        result.is_none(),
        "latest-activity query must return None when no sessions exist for the agent"
    );
}

/// T3: heartbeat-session query returns Some only for sessions with channel='heartbeat'.
#[sqlx::test(migrations = "../../migrations")]
async fn heartbeat_query_ignores_non_heartbeat_sessions(pool: PgPool) {
    let now = Utc::now();
    // Insert a non-heartbeat session — must NOT count.
    sqlx::query(
        "INSERT INTO sessions \
         (id, agent_id, user_id, channel, started_at, last_message_at, activity_at, run_status) \
         VALUES (gen_random_uuid(), 'HbAgent', 'u', 'web', $1, $1, $1, 'done')",
    )
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert web session");

    let result_no_hb: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT MAX(started_at) FROM sessions \
         WHERE agent_id = $1 AND channel = 'heartbeat'",
    )
    .bind("HbAgent")
    .fetch_one(&pool)
    .await
    .expect("query before heartbeat session");

    assert!(
        result_no_hb.is_none(),
        "heartbeat query must return None when only non-heartbeat sessions exist"
    );

    // Insert a heartbeat session — now must return Some.
    sqlx::query(
        "INSERT INTO sessions \
         (id, agent_id, user_id, channel, started_at, last_message_at, run_status) \
         VALUES (gen_random_uuid(), 'HbAgent', 'u', 'heartbeat', $1, $1, 'done')",
    )
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert heartbeat session");

    let result_with_hb: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT MAX(started_at) FROM sessions \
         WHERE agent_id = $1 AND channel = 'heartbeat'",
    )
    .bind("HbAgent")
    .fetch_one(&pool)
    .await
    .expect("query after heartbeat session");

    assert!(
        result_with_hb.is_some(),
        "heartbeat query must return Some after a heartbeat session is inserted"
    );
}

/// T4: NULL activity_at falls back to last_message_at via COALESCE.
#[sqlx::test(migrations = "../../migrations")]
async fn null_activity_at_falls_back_to_last_message_at(pool: PgPool) {
    // Spec contract: a session with NULL activity_at should not prevent
    // latest_activity_at from being reported — last_message_at serves as
    // a fallback. The SQL uses COALESCE(GREATEST(MAX(a), MAX(l)), MAX(a), MAX(l))
    // for that fallback.
    let lm = chrono::Utc::now() - chrono::Duration::hours(1);
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel, started_at, last_message_at, activity_at, run_status) \
         VALUES (gen_random_uuid(), 'NullActAgent', 'u', 'web', $1, $1, NULL, 'done')",
    )
    .bind(lm)
    .execute(&pool)
    .await
    .expect("insert session with null activity_at");

    let result: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT COALESCE( \
             GREATEST(MAX(activity_at), MAX(last_message_at)), \
             MAX(activity_at), \
             MAX(last_message_at)) \
         FROM sessions WHERE agent_id = $1",
    )
    .bind("NullActAgent")
    .fetch_one(&pool)
    .await
    .expect("query");
    assert!(result.is_some(), "NULL activity_at must fall back to last_message_at");
    assert_eq!(
        result.unwrap().timestamp(),
        lm.timestamp(),
        "fallback value must be last_message_at"
    );
}
