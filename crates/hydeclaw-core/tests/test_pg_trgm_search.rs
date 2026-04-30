//! Integration tests for pg_trgm-based search.
//! Requires DATABASE_URL pointing at a Postgres with pg_trgm extension.

use hydeclaw_core::db::memory_queries;
use sqlx::PgPool;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    PgPool::connect(&url).await.expect("connect")
}

async fn cleanup(db: &PgPool, prefix: &str) {
    sqlx::query("DELETE FROM memory_chunks WHERE content LIKE $1")
        .bind(format!("{}%", prefix))
        .execute(db)
        .await
        .ok();
}

async fn insert_chunk(db: &PgPool, content: &str, agent_id: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO memory_chunks (id, content, source, pinned, scope, agent_id) \
         VALUES ($1::uuid, $2, 'test', false, 'private', $3)",
    )
    .bind(&id)
    .bind(content)
    .bind(agent_id)
    .execute(db)
    .await
    .expect("insert");
    id
}

#[tokio::test]
async fn trigram_finds_russian_partial_match() {
    let db = pool().await;
    let agent = format!("test-trg-rus-{}", uuid::Uuid::new_v4());
    cleanup(&db, "TRG_TEST_RUS_").await;

    insert_chunk(&db, "TRG_TEST_RUS_пользователь", &agent).await;
    insert_chunk(&db, "TRG_TEST_RUS_пользователи", &agent).await;
    insert_chunk(&db, "TRG_TEST_RUS_user_account", &agent).await;

    let results = memory_queries::search_trigram(&db, "пользоват", 10, 0.3, &agent)
        .await
        .expect("search_trigram");

    let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert!(
        contents.iter().any(|c| c.contains("пользователь")),
        "expected пользователь, got: {:?}", contents
    );
    assert!(
        contents.iter().any(|c| c.contains("пользователи")),
        "expected пользователи, got: {:?}", contents
    );
    assert!(
        !contents.iter().any(|c| c.contains("user_account")),
        "user_account must NOT match query 'пользоват', got: {:?}", contents
    );

    cleanup(&db, "TRG_TEST_RUS_").await;
}

#[tokio::test]
async fn trigram_finds_cjk_substring() {
    let db = pool().await;
    let agent = format!("test-trg-cjk-{}", uuid::Uuid::new_v4());
    cleanup(&db, "TRG_TEST_CJK_").await;

    insert_chunk(&db, "TRG_TEST_CJK_東京タワー", &agent).await;
    insert_chunk(&db, "TRG_TEST_CJK_大阪城", &agent).await;

    let results = memory_queries::search_trigram(&db, "東京", 10, 0.2, &agent)
        .await
        .expect("search_trigram CJK");

    let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert!(
        contents.iter().any(|c| c.contains("東京タワー")),
        "expected 東京タワー match for query 東京, got: {:?}", contents
    );

    cleanup(&db, "TRG_TEST_CJK_").await;
}

#[tokio::test]
async fn trigram_handles_typo() {
    let db = pool().await;
    let agent = format!("test-trg-typo-{}", uuid::Uuid::new_v4());
    cleanup(&db, "TRG_TEST_TYPO_").await;

    insert_chunk(&db, "TRG_TEST_TYPO_пользователь", &agent).await;

    // Опечатка: "пользоветель" вместо "пользователь" (одна буква отличается).
    let results = memory_queries::search_trigram(&db, "пользоветель", 10, 0.4, &agent)
        .await
        .expect("search_trigram typo");

    let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert!(
        contents.iter().any(|c| c.contains("пользователь")),
        "typo 'пользоветель' should still match 'пользователь', got: {:?}", contents
    );

    cleanup(&db, "TRG_TEST_TYPO_").await;
}

#[tokio::test]
async fn trigram_threshold_filters_garbage() {
    let db = pool().await;
    let agent = format!("test-trg-thr-{}", uuid::Uuid::new_v4());
    cleanup(&db, "TRG_TEST_THR_").await;

    insert_chunk(&db, "TRG_TEST_THR_хороший контент про пингвинов", &agent).await;

    // Single character query → должно вернуть пусто при threshold 0.3.
    let results = memory_queries::search_trigram(&db, "x", 10, 0.3, &agent)
        .await
        .expect("search_trigram threshold");

    assert!(
        results.is_empty(),
        "single-char query should not match anything at threshold 0.3, got {} results",
        results.len()
    );

    cleanup(&db, "TRG_TEST_THR_").await;
}
