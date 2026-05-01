//! Integration tests for extended token tracking in usage_log.
//! Each test gets its own fresh migrated DB via `#[sqlx::test]`.
//! Gated to Linux x86_64 because testcontainers requires Docker.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use hydeclaw_db::usage;
use hydeclaw_types::TokenUsage;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn record_usage_persists_extended_fields(db: PgPool) {
    let test_agent = format!("test-tok-{}", uuid::Uuid::new_v4());

    let usage = TokenUsage {
        input_tokens: 12500,
        output_tokens: 1800,
        cache_read_tokens: Some(8200),
        cache_creation_tokens: Some(1200),
        reasoning_tokens: Some(600),
    };
    usage::record_usage(
        &db,
        &test_agent,
        "anthropic",
        "claude-sonnet-4-6",
        None, // session_id
        &usage,
    )
    .await
    .expect("record_usage");

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
}

#[sqlx::test(migrations = "../../migrations")]
async fn record_usage_legacy_none_fields(db: PgPool) {
    let test_agent = format!("test-tok-legacy-{}", uuid::Uuid::new_v4());

    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        reasoning_tokens: None,
    };
    usage::record_usage(&db, &test_agent, "openai", "gpt-4", None, &usage)
        .await
        .expect("legacy record_usage");

    let row: (Option<i32>, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT cache_read_tokens, cache_creation_tokens, reasoning_tokens
         FROM usage_log WHERE agent_id = $1 ORDER BY created_at DESC LIMIT 1"
    )
    .bind(&test_agent)
    .fetch_one(&db).await.expect("select");

    assert_eq!(row.0, None);
    assert_eq!(row.1, None);
    assert_eq!(row.2, None);
}
