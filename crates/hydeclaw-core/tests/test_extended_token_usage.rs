//! Integration tests for extended token tracking in usage_log.

use hydeclaw_db::usage;
use sqlx::PgPool;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    PgPool::connect(&url).await.expect("connect")
}

#[tokio::test]
async fn record_usage_persists_extended_fields() {
    let db = pool().await;
    let test_agent = format!("test-tok-{}", uuid::Uuid::new_v4());

    usage::record_usage(
        &db,
        &test_agent,
        "anthropic",
        "claude-sonnet-4-6",
        12500,                  // input_tokens
        1800,                   // output_tokens
        None,                   // session_id
        Some(8200),             // cache_read_tokens (NEW)
        Some(1200),             // cache_creation_tokens (NEW)
        Some(600),              // reasoning_tokens (NEW)
    ).await.expect("record_usage");

    let row: (i32, i32, Option<i32>, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, reasoning_tokens
         FROM usage_log WHERE agent_id = $1 ORDER BY created_at DESC LIMIT 1"
    )
    .bind(&test_agent)
    .fetch_one(&db)
    .await
    .expect("select");

    assert_eq!(row.0, 12500);
    assert_eq!(row.1, 1800);
    assert_eq!(row.2, Some(8200));
    assert_eq!(row.3, Some(1200));
    assert_eq!(row.4, Some(600));

    sqlx::query("DELETE FROM usage_log WHERE agent_id = $1")
        .bind(&test_agent)
        .execute(&db).await.ok();
}

#[tokio::test]
async fn record_usage_legacy_none_fields() {
    let db = pool().await;
    let test_agent = format!("test-tok-legacy-{}", uuid::Uuid::new_v4());

    usage::record_usage(
        &db, &test_agent, "openai", "gpt-4",
        100, 50, None,
        None, None, None,  // все extended поля = None
    ).await.expect("legacy record_usage");

    let row: (Option<i32>, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT cache_read_tokens, cache_creation_tokens, reasoning_tokens
         FROM usage_log WHERE agent_id = $1 ORDER BY created_at DESC LIMIT 1"
    )
    .bind(&test_agent)
    .fetch_one(&db).await.expect("select");

    assert_eq!(row.0, None);
    assert_eq!(row.1, None);
    assert_eq!(row.2, None);

    sqlx::query("DELETE FROM usage_log WHERE agent_id = $1")
        .bind(&test_agent)
        .execute(&db).await.ok();
}
