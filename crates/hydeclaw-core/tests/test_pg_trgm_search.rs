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
