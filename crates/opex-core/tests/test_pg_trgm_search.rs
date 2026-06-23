//! Integration tests for pg_trgm-based search.
//! Each test gets its own fresh migrated DB via `#[sqlx::test]`.
//! Gated to Linux x86_64 because testcontainers requires Docker.
//!
//! NOTE: `search_hybrid` 3-way RRF coverage (review fix C4) is intentionally
//! NOT included here. It would require importing `opex_core::memory::{MemoryStore,
//! EmbeddingService}`, but the `memory` module is not part of the lib facade
//! (see `crates/opex-core/src/lib.rs` — surface is capped at ~10 modules
//! to prevent the facade from becoming a parallel module tree). Adding it would
//! exceed scope of this review-fix and was explicitly out-of-bounds per the
//! "do NOT introduce new public surface" constraint. C4 is tracked as a
//! follow-up: either expose `memory` via a leaf-only facade, or test
//! `search_hybrid` indirectly via the chat HTTP API in a higher-level
//! integration test.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use opex_db::memory_queries;
use sqlx::PgPool;

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

#[sqlx::test(migrations = "../../migrations")]
async fn trigram_finds_russian_partial_match(db: PgPool) {
    let agent = format!("test-trg-rus-{}", uuid::Uuid::new_v4());

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

    // Cleanup is automatic — sqlx::test drops the per-test DB.
    // Explicit DELETE-by-agent_id kept as belt-and-braces for parallel safety.
    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent)
        .execute(&db)
        .await
        .ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn trigram_finds_cjk_substring(db: PgPool) {
    let agent = format!("test-trg-cjk-{}", uuid::Uuid::new_v4());

    insert_chunk(&db, "TRG_TEST_CJK_東京タワー", &agent).await;
    insert_chunk(&db, "TRG_TEST_CJK_大阪城", &agent).await;

    // Threshold 0.05 — 2 CJK chars vs full string (with "TRG_TEST_CJK_" prefix)
    // produce few common trigrams (byte-level trigrams over multi-byte chars +
    // prefix dilution). Verified empirically — CI Postgres pg_trgm reports
    // similarity ~0.06-0.10 for this pair.
    let results = memory_queries::search_trigram(&db, "東京", 10, 0.05, &agent)
        .await
        .expect("search_trigram CJK");

    let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert!(
        contents.iter().any(|c| c.contains("東京タワー")),
        "expected 東京タワー match for query 東京, got: {:?}", contents
    );

    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent).execute(&db).await.ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn trigram_handles_typo(db: PgPool) {
    let agent = format!("test-trg-typo-{}", uuid::Uuid::new_v4());

    insert_chunk(&db, "TRG_TEST_TYPO_пользователь", &agent).await;

    // Опечатка: "пользоветель" vs "пользователь" (одна буква отличается).
    // Threshold 0.2 — full insertion has "TRG_TEST_TYPO_" prefix (~28 chars
    // total) vs 12-char typo query. Prefix dilutes trigram overlap to
    // ~0.30 similarity per pg_trgm computation. 0.4 was too aggressive.
    let results = memory_queries::search_trigram(&db, "пользоветель", 10, 0.2, &agent)
        .await
        .expect("search_trigram typo");

    let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert!(
        contents.iter().any(|c| c.contains("пользователь")),
        "typo 'пользоветель' should still match 'пользователь', got: {:?}", contents
    );

    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent).execute(&db).await.ok();
}

#[sqlx::test(migrations = "../../migrations")]
async fn trigram_threshold_filters_garbage(db: PgPool) {
    let agent = format!("test-trg-thr-{}", uuid::Uuid::new_v4());

    insert_chunk(&db, "TRG_TEST_THR_хороший контент про пингвинов", &agent).await;

    let results = memory_queries::search_trigram(&db, "x", 10, 0.3, &agent)
        .await
        .expect("search_trigram threshold");

    assert!(
        results.is_empty(),
        "single-char query should not match anything at threshold 0.3, got {} results",
        results.len()
    );

    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent).execute(&db).await.ok();
}
