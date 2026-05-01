//! Integration tests for `MemoryStore::search_hybrid` 3-way RRF combining
//! (Phase 2 P0.4 follow-up to PR #22).
//!
//! PR #22 added the trigram branch and the 8-state shortcut match in
//! `crates/hydeclaw-core/src/memory/store.rs::search_hybrid`. The pre-existing
//! `tests/test_pg_trgm_search.rs` only exercises `search_trigram` directly —
//! the combiner itself had zero coverage.
//!
//! These tests reach `MemoryStore` via the `memory_test_facade` (see
//! `crates/hydeclaw-core/src/lib.rs`), a `#[doc(hidden)]` exception to the
//! Phase 61 lib-facade cap. The facade exposes exactly four names
//! (`MemoryStore`, `EmbeddingService`, `MemoryResult`, `MemoryChunk`) — no
//! production code paths import from it.
//!
//! Each test gets its own fresh migrated DB via `#[sqlx::test]`. Gated to
//! Linux x86_64 because testcontainers requires Docker.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use async_trait::async_trait;
use hydeclaw_core::memory_test_facade::{EmbeddingService, MemoryStore};
use sqlx::PgPool;
use std::sync::Arc;

// ── Fake embedder ────────────────────────────────────────────────────────────

/// Returns a fixed 4-dimensional vector for every input. The semantic branch
/// of `search_hybrid` ranks by cosine distance — when every chunk has the
/// same embedding, every chunk has identical similarity to the query, so the
/// branch contributes a stable (but un-discriminating) ranking. That's
/// exactly what we want here: the test asserts that the *combiner* runs and
/// the *shortcut paths* return correctly, not that the embedding model is
/// any good.
struct FakeEmbedder;

#[async_trait]
impl EmbeddingService for FakeEmbedder {
    fn is_available(&self) -> bool {
        true
    }

    fn embed_dim(&self) -> u32 {
        4
    }

    fn embed_model_name(&self) -> Option<String> {
        Some("fake".to_string())
    }

    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.5, 0.5, 0.5, 0.5])
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok((0..texts.len()).map(|_| vec![0.5, 0.5, 0.5, 0.5]).collect())
    }
}

/// Embedder that reports `is_available() == false`. Forces `MemoryStore::search`
/// to take the FTS-only fallback branch — used to verify the combiner shortcut
/// gating.
struct DisabledEmbedder;

#[async_trait]
impl EmbeddingService for DisabledEmbedder {
    fn is_available(&self) -> bool {
        false
    }

    fn embed_dim(&self) -> u32 {
        0
    }

    fn embed_model_name(&self) -> Option<String> {
        None
    }

    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("embedding unavailable")
    }

    async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("embedding unavailable")
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Insert a chunk with an embedding so all three search branches (semantic +
/// FTS + trigram) can match it. Vector dim must match `FakeEmbedder::embed`.
async fn insert_chunk_with_embedding(db: &PgPool, content: &str, agent_id: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO memory_chunks (id, content, source, pinned, scope, agent_id, embedding) \
         VALUES ($1::uuid, $2, 'test', false, 'private', $3, $4::vector)",
    )
    .bind(&id)
    .bind(content)
    .bind(agent_id)
    .bind("[0.5,0.5,0.5,0.5]")
    .execute(db)
    .await
    .expect("insert chunk with embedding");
    id
}

/// Insert a chunk without an embedding — semantic branch will skip it via
/// `WHERE embedding IS NOT NULL`, but FTS + trigram still match. Used to
/// drive the `(true, false, false)` and similar shortcut arms.
async fn insert_chunk_no_embedding(db: &PgPool, content: &str, agent_id: &str) -> String {
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
    .expect("insert chunk without embedding");
    id
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Fires the full RRF combiner: every branch returns at least one chunk, so
/// the 8-state shortcut falls through to the actual rank-fusion code path.
#[sqlx::test(migrations = "../../migrations")]
async fn search_hybrid_returns_results_when_all_three_branches_match(db: PgPool) {
    let agent = format!("test-rrf-all-{}", uuid::Uuid::new_v4());

    // All three chunks have FTS + trigram + semantic coverage.
    insert_chunk_with_embedding(&db, "RRF_TEST_пользователь данные", &agent).await;
    insert_chunk_with_embedding(&db, "RRF_TEST_пользователи система", &agent).await;
    insert_chunk_with_embedding(&db, "RRF_TEST_пользоват_partial_match", &agent).await;

    let store = MemoryStore::new(
        db.clone(),
        Arc::new(FakeEmbedder),
        "russian".to_string(),
    );

    let (results, mode) = store
        .search("пользоват", 10, &[], &agent)
        .await
        .expect("search");

    assert_eq!(
        mode, "hybrid",
        "expected hybrid mode when every branch matches, got {mode}"
    );
    assert!(
        !results.is_empty(),
        "RRF combiner must return at least one result"
    );
    let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert!(
        contents.iter().any(|c| c.contains("RRF_TEST_")),
        "results should include the test chunks, got: {contents:?}"
    );

    // Belt-and-braces cleanup (sqlx::test drops the DB anyway).
    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent)
        .execute(&db)
        .await
        .ok();
}

/// Empty query short-circuits the entire pipeline before any branch runs.
/// Verifies the early-return at `store.rs::search` line 97-99.
#[sqlx::test(migrations = "../../migrations")]
async fn search_hybrid_empty_query_returns_empty(db: PgPool) {
    let agent = format!("test-rrf-empty-{}", uuid::Uuid::new_v4());
    insert_chunk_with_embedding(&db, "RRF_EMPTY_data", &agent).await;

    let store = MemoryStore::new(
        db.clone(),
        Arc::new(FakeEmbedder),
        "russian".to_string(),
    );
    let (results, mode) = store.search("", 5, &[], &agent).await.expect("search empty");

    assert!(results.is_empty(), "empty query must return no results");
    assert_eq!(mode, "none", "empty query must report mode='none'");
}

/// Trigram-only path: chunk has no embedding (semantic skips it) and the
/// query is a typo (FTS plainto_tsquery misses the morphological match
/// because `пользоветель` is not a real word — it doesn't stem to the
/// indexed form). Trigram fuzzy match is the only branch that fires, so
/// the `(true, true, false)` shortcut arm at store.rs:171 returns it
/// directly without RRF.
#[sqlx::test(migrations = "../../migrations")]
async fn search_hybrid_returns_results_for_typo_recovery(db: PgPool) {
    let agent = format!("test-rrf-typo-{}", uuid::Uuid::new_v4());
    insert_chunk_no_embedding(&db, "RRF_TYPO_пользоветель", &agent).await;

    let store = MemoryStore::new(
        db.clone(),
        Arc::new(FakeEmbedder),
        "russian".to_string(),
    );

    // Typo'd query — the indexed form is "пользоветель"; queryng with the
    // canonical "пользователь" only matches via trigram fuzzy similarity.
    let (results, _mode) = store
        .search("пользователь", 5, &[], &agent)
        .await
        .expect("search typo");

    let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert!(
        contents.iter().any(|c| c.contains("пользоветель")),
        "trigram branch must surface the typo'd chunk, got: {contents:?}"
    );

    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent)
        .execute(&db)
        .await
        .ok();
}

/// Disabled embedder → `is_available() == false` → `search` skips the hybrid
/// combiner entirely and falls through to the pure-FTS path. Verifies the
/// embedder gate at `store.rs::search` line 101.
#[sqlx::test(migrations = "../../migrations")]
async fn search_hybrid_skipped_when_embedder_unavailable(db: PgPool) {
    let agent = format!("test-rrf-no-embed-{}", uuid::Uuid::new_v4());
    insert_chunk_with_embedding(&db, "RRF_NOEMBED_данные системы", &agent).await;

    let store = MemoryStore::new(
        db.clone(),
        Arc::new(DisabledEmbedder),
        "russian".to_string(),
    );

    let (results, mode) = store
        .search("данные", 5, &[], &agent)
        .await
        .expect("search no-embed");

    assert_eq!(
        mode, "fts",
        "disabled embedder must force FTS-only mode, got {mode}"
    );
    assert!(
        !results.is_empty(),
        "FTS branch alone must surface the matching chunk"
    );

    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent)
        .execute(&db)
        .await
        .ok();
}
