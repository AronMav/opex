//! Integration tests for compression chain split (P1.1).
//! Requires DATABASE_URL — skipped automatically when not set.

use uuid::Uuid;

/// Get a test DB pool. Returns None if DATABASE_URL is not set.
async fn test_db() -> Option<sqlx::PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .ok()
}

/// Insert a bare session row for testing.
async fn insert_test_session(db: &sqlx::PgPool, agent_id: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel)
         VALUES ($1, $2, 'test_user', 'test')",
    )
    .bind(id)
    .bind(agent_id)
    .execute(db)
    .await
    .expect("insert session");
    id
}

#[tokio::test]
async fn no_split_when_pending_split_false() {
    let db = match test_db().await { Some(d) => d, None => return };

    // Use a unique agent_id so parallel tests don't affect this COUNT.
    let agent_id = format!("test-no-split-{}", Uuid::new_v4());
    let session_id = insert_test_session(&db, &agent_id).await;
    let state = serde_json::json!({
        "previous_summary": "some summary",
        "ineffective_count": 0,
        "compression_count": 1,
        "pending_split": false
    });
    // set_compaction_state is not exposed via lib.rs facade — use raw SQL directly
    sqlx::query("UPDATE sessions SET compaction_state = $1 WHERE id = $2")
        .bind(&state)
        .bind(session_id)
        .execute(&db)
        .await
        .unwrap();

    let count_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE agent_id = $1"
    )
    .bind(&agent_id)
    .fetch_one(&db).await.unwrap();

    let row = hydeclaw_db::sessions::get_session_for_chain(&db, session_id)
        .await.unwrap();
    assert!(row.is_some());

    let count_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE agent_id = $1"
    )
    .bind(&agent_id)
    .fetch_one(&db).await.unwrap();
    assert_eq!(count_before, count_after, "no new session created");
}

#[tokio::test]
async fn create_chain_session_links_parent() {
    let db = match test_db().await { Some(d) => d, None => return };

    let parent_id = insert_test_session(&db, "TestAgent").await;
    let child_id = hydeclaw_db::sessions::create_chain_session(
        &db, parent_id, "TestAgent", "user1", "ui", Some("Test Session")
    ).await.unwrap();

    let (parent_fk,): (Option<Uuid>,) = sqlx::query_as(
        "SELECT parent_session_id FROM sessions WHERE id = $1"
    )
    .bind(child_id)
    .fetch_one(&db).await.unwrap();
    assert_eq!(parent_fk, Some(parent_id));
}

#[tokio::test]
async fn set_session_end_reason_updates_parent() {
    let db = match test_db().await { Some(d) => d, None => return };

    let session_id = insert_test_session(&db, "TestAgent").await;
    hydeclaw_db::sessions::set_session_end_reason(&db, session_id, "compression")
        .await.unwrap();

    let (end_reason,): (Option<String>,) = sqlx::query_as(
        "SELECT end_reason FROM sessions WHERE id = $1"
    )
    .bind(session_id)
    .fetch_one(&db).await.unwrap();
    assert_eq!(end_reason.as_deref(), Some("compression"));
}

#[tokio::test]
async fn get_session_chain_returns_ancestors_root_first() {
    let db = match test_db().await { Some(d) => d, None => return };

    // Build chain A -> B -> C
    let a = insert_test_session(&db, "TestAgent").await;
    let b = hydeclaw_db::sessions::create_chain_session(&db, a, "TestAgent", "u", "ui", None).await.unwrap();
    let c = hydeclaw_db::sessions::create_chain_session(&db, b, "TestAgent", "u", "ui", None).await.unwrap();

    let chain = hydeclaw_db::sessions::get_session_chain(&db, c).await.unwrap();
    assert_eq!(chain.len(), 3, "chain has 3 sessions");
    assert_eq!(chain[0].id, a, "root (A) is first");
    assert_eq!(chain[1].id, b);
    assert_eq!(chain[2].id, c, "current (C) is last");
    assert_eq!(chain[2].depth, 0);
    assert_eq!(chain[0].depth, 2);
}

#[tokio::test]
async fn insert_seed_messages_preserves_order() {
    let db = match test_db().await { Some(d) => d, None => return };

    let session_id = insert_test_session(&db, "TestAgent").await;

    let messages = vec![
        hydeclaw_types::Message {
            role: hydeclaw_types::MessageRole::System,
            content: "sys".into(),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
            db_id: None,
        },
        hydeclaw_types::Message {
            role: hydeclaw_types::MessageRole::Assistant,
            content: "summary".into(),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
            db_id: None,
        },
        hydeclaw_types::Message {
            role: hydeclaw_types::MessageRole::User,
            content: "user turn".into(),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
            db_id: None,
        },
    ];

    hydeclaw_db::sessions::insert_seed_messages(&db, session_id, "TestAgent", &messages)
        .await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT role FROM messages WHERE session_id = $1 ORDER BY created_at ASC"
    )
    .bind(session_id)
    .fetch_all(&db).await.unwrap();

    let roles: Vec<&str> = rows.iter().map(|(r,)| r.as_str()).collect();
    assert_eq!(roles, vec!["system", "assistant", "user"]);
}

// ── Unit 4: child session compaction_state reset ────────────────────────────

/// After `create_chain_session`, the child's `compaction_state` column in DB
/// must be NULL.  The actual state reset (inheriting previous_summary, zeroing
/// counters) happens in bootstrap.rs when the child is first loaded — so the
/// row itself starts clean.
#[tokio::test]
async fn child_session_has_null_compaction_state_initially() {
    let db = match test_db().await { Some(d) => d, None => return };

    let parent_id = insert_test_session(&db, "TestAgent").await;

    // Give parent a non-trivial compaction_state so we can confirm the child
    // doesn't accidentally copy it.
    let parent_state = serde_json::json!({
        "previous_summary": "summary text",
        "ineffective_count": 3,
        "compression_count": 5,
        "pending_split": true
    });
    sqlx::query("UPDATE sessions SET compaction_state = $1 WHERE id = $2")
        .bind(&parent_state)
        .bind(parent_id)
        .execute(&db)
        .await
        .unwrap();

    let child_id = hydeclaw_db::sessions::create_chain_session(
        &db, parent_id, "TestAgent", "user1", "ui", Some("Child Session"),
    ).await.unwrap();

    let (compaction_state,): (Option<serde_json::Value>,) = sqlx::query_as(
        "SELECT compaction_state FROM sessions WHERE id = $1"
    )
    .bind(child_id)
    .fetch_one(&db)
    .await
    .unwrap();

    assert!(
        compaction_state.is_none(),
        "child compaction_state should be NULL immediately after create_chain_session; \
         the reset (with inherited previous_summary) happens in bootstrap.rs"
    );
}
