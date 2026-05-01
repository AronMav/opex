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

/// Discriminating embedder: maps anchor keywords to distinct unit vectors so the
/// semantic branch produces a meaningful ranking. Without this, every chunk has
/// identical cosine similarity to the query and the semantic branch contributes
/// only positional noise to RRF — making it impossible to assert that the
/// combiner actually fuses three independent rankings.
struct KeywordEmbedder;

#[async_trait]
impl EmbeddingService for KeywordEmbedder {
    fn is_available(&self) -> bool {
        true
    }

    fn embed_dim(&self) -> u32 {
        4
    }

    fn embed_model_name(&self) -> Option<String> {
        Some("keyword-fake".to_string())
    }

    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        // Each anchor word produces a distinct axis-aligned unit vector. A query
        // containing exactly one anchor scores ≈1.0 cosine with chunks tagged by
        // the same anchor and 0.0 with chunks tagged by a different anchor.
        let v = if text.contains("RRF_ALPHA") {
            vec![1.0_f32, 0.0, 0.0, 0.0]
        } else if text.contains("RRF_BETA") {
            vec![0.0_f32, 1.0, 0.0, 0.0]
        } else if text.contains("RRF_GAMMA") {
            vec![0.0_f32, 0.0, 1.0, 0.0]
        } else {
            vec![0.0_f32, 0.0, 0.0, 1.0]
        };
        Ok(v)
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
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
///
/// `tsv` MUST be populated explicitly here — production code does this via
/// `to_tsvector('russian', content)` in `memory_queries::index`, but the
/// `memory_chunks.tsv` column is a plain `tsvector` (not GENERATED), so
/// without setting it the FTS branch silently returns empty for every chunk.
async fn insert_chunk_with_embedding(db: &PgPool, content: &str, agent_id: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO memory_chunks (id, content, source, pinned, scope, agent_id, embedding, tsv) \
         VALUES ($1::uuid, $2, 'test', false, 'private', $3, $4::vector, to_tsvector('russian', $2))",
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

/// Insert a chunk with a custom 4-dim embedding vector. Used by tests that
/// need the semantic branch to discriminate between chunks.
async fn insert_chunk_with_vec(
    db: &PgPool,
    content: &str,
    agent_id: &str,
    embedding: [f32; 4],
) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let vec_str = format!(
        "[{},{},{},{}]",
        embedding[0], embedding[1], embedding[2], embedding[3]
    );
    sqlx::query(
        "INSERT INTO memory_chunks (id, content, source, pinned, scope, agent_id, embedding, tsv) \
         VALUES ($1::uuid, $2, 'test', false, 'private', $3, $4::vector, to_tsvector('russian', $2))",
    )
    .bind(&id)
    .bind(content)
    .bind(agent_id)
    .bind(&vec_str)
    .execute(db)
    .await
    .expect("insert chunk with custom embedding");
    id
}

/// Insert a chunk without an embedding — semantic branch will skip it via
/// `WHERE embedding IS NOT NULL`, but FTS + trigram still match. Used to
/// drive the `(true, false, false)` and similar shortcut arms.
async fn insert_chunk_no_embedding(db: &PgPool, content: &str, agent_id: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO memory_chunks (id, content, source, pinned, scope, agent_id, tsv) \
         VALUES ($1::uuid, $2, 'test', false, 'private', $3, to_tsvector('russian', $2))",
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

/// RRF fusion math: a chunk that ranks in 2 of 3 layers must outrank a chunk
/// that ranks only in 1 layer. Locks in that layer-stacking is rewarded —
/// dropping a layer's contribution or inverting weights would let a single-layer
/// chunk win, breaking the test.
///
/// Setup (chunk embeddings set explicitly via `insert_chunk_with_vec` to side-
/// step ambiguity in `KeywordEmbedder`'s anchor mapping):
/// - `multi`    embed=[1,0,0,0], content "контекст система winner" — SEM cos=1.0
///              with query, FTS matches both query lexemes
/// - `sem_only` embed=[1,0,0,0], content "qwertyuiop_xyz" — SEM cos=1.0 too,
///              but no query lexeme overlap so FTS skips it
/// - `fts_only` embed=[0,1,0,0], content "контекст другое" — orthogonal
///              embedding (SEM cos=0), one query lexeme matches FTS
///
/// Query: "контекст система" → KeywordEmbedder default branch returns [0,0,0,1]
/// — wait, we want query embedding aligned with `multi`/`sem_only`. We override
/// by adding "RRF_ALPHA" to the query string so the embedder picks [1,0,0,0].
///
/// Expected RRF (assuming rank 0 wherever present):
/// - multi:    W_SEM/61 + W_FTS/61 = 0.85/61 ≈ 0.01393
/// - sem_only: W_SEM/61            = 0.6/61  ≈ 0.00984
/// - fts_only: W_FTS/61            = 0.25/61 ≈ 0.00410
///
/// Even if SEM tie-break puts `multi` at rank 1 (worst case): 0.6/62 + 0.25/61
/// = 0.01378 — still beats `sem_only` at best 0.00984. Asserting `multi` is
/// top-1 catches (a) any single-layer skip, (b) W_SEM ↔ W_FTS inversion that
/// would demote 2-of-3 chunks below 1-of-3 chunks.
#[sqlx::test(migrations = "../../migrations")]
async fn search_hybrid_rewards_multi_layer_chunks(db: PgPool) {
    let agent = format!("test-rrf-fusion-{}", uuid::Uuid::new_v4());

    // SEM-only: explicit alpha-aligned embedding, content has no Russian
    // lexemes the query tokenizes to (so FTS branch returns zero hits) and no
    // shared trigrams above 0.3 threshold (so TRGM branch skips it).
    let sem_only = insert_chunk_with_vec(
        &db,
        "qwertyuiop_xyz_marker_unique",
        &agent,
        [1.0, 0.0, 0.0, 0.0],
    )
    .await;

    // FTS-only: orthogonal embedding zeros SEM contribution, "контекст"
    // lexeme matches plainto_tsquery.
    let fts_only = insert_chunk_with_vec(
        &db,
        "контекст другое значение",
        &agent,
        [0.0, 1.0, 0.0, 0.0],
    )
    .await;

    // Multi-layer: alpha-aligned embedding (SEM cos=1.0) AND both query lexemes.
    let multi = insert_chunk_with_vec(
        &db,
        "контекст система winner_chunk",
        &agent,
        [1.0, 0.0, 0.0, 0.0],
    )
    .await;

    let store = MemoryStore::new(
        db.clone(),
        Arc::new(KeywordEmbedder),
        "russian".to_string(),
    );

    // Inject "RRF_ALPHA" into the query so KeywordEmbedder maps it to [1,0,0,0].
    // FTS still tokenizes "контекст система" — RRF_ALPHA is just a Latin token
    // that won't match any chunk content (none contain it).
    let (results, mode) = store
        .search("RRF_ALPHA контекст система", 5, &[], &agent)
        .await
        .expect("hybrid search");

    assert_eq!(mode, "hybrid", "all branches non-empty must pick hybrid mode");
    assert!(
        !results.is_empty(),
        "expected at least one result, got {}",
        results.len()
    );

    let top_id = &results[0].id;
    assert_eq!(
        top_id, &multi,
        "multi-layer chunk must rank #1 over single-layer chunks (RRF math broken?)\n\
         Top: {top_id}\nMulti: {multi}\nSemOnly: {sem_only}\nFtsOnly: {fts_only}"
    );

    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
        .bind(&agent)
        .execute(&db)
        .await
        .ok();
}

/// Determinism guard: HashMap::into_values() iteration order is randomized
/// under Rust's default hasher, so without an explicit secondary sort key the
/// final RRF ranking flakes whenever scores tie. Run search_hybrid five times
/// and assert identical top-N across runs.
#[sqlx::test(migrations = "../../migrations")]
async fn search_hybrid_results_are_deterministic_under_ties(db: PgPool) {
    let agent = format!("test-rrf-det-{}", uuid::Uuid::new_v4());

    // Three chunks with identical FTS / TRGM signal and orthogonal embeddings
    // — every chunk lands in the same per-layer rank position, so RRF scores
    // are bit-equal and ordering depends entirely on the sort tie-break.
    insert_chunk_with_vec(&db, "система данные", &agent, [1.0, 0.0, 0.0, 0.0]).await;
    insert_chunk_with_vec(&db, "система данные", &agent, [0.0, 1.0, 0.0, 0.0]).await;
    insert_chunk_with_vec(&db, "система данные", &agent, [0.0, 0.0, 1.0, 0.0]).await;

    let store = MemoryStore::new(
        db.clone(),
        Arc::new(KeywordEmbedder),
        "russian".to_string(),
    );

    let mut runs: Vec<Vec<String>> = Vec::with_capacity(5);
    for _ in 0..5 {
        let (results, _mode) = store
            .search("система данные", 5, &[], &agent)
            .await
            .expect("hybrid search");
        runs.push(results.iter().map(|r| r.id.clone()).collect());
    }

    let first = &runs[0];
    for (i, r) in runs.iter().enumerate() {
        assert_eq!(
            r, first,
            "run {i} ordering diverged from run 0 — RRF tie-break is non-deterministic"
        );
    }

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
    // Note: tokens MUST be space-separated. The Russian FTS dictionary keeps
    // underscore-joined runs as a single token (e.g., "RRF_NOEMBED_данные"
    // does NOT stem to lemma "дан"), so "RRF_NOEMBED_данные системы" would
    // never match the query "данные". Pre-existing bug in commit 8fa0482.
    insert_chunk_with_embedding(&db, "RRF NOEMBED данные системы", &agent).await;

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
