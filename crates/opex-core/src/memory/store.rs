/// `MemoryStore` — the main entry point for memory operations (search, index, get, delete).
///
/// Delegates embedding to an `Arc<dyn EmbeddingService>` and executes queries
/// against PostgreSQL via `crate::db::memory_queries`.
use anyhow::{Context, Result};
use sqlx::PgPool;
use std::sync::{Arc, RwLock};

use super::embedding::{fmt_vec, EmbeddingService};
use super::{MemoryChunk, MemoryResult};

// ── Hybrid search RRF tuning ────────────────────────────────────────────
// Reciprocal Rank Fusion weights for the three search branches.
// Updated 2026-04-30: trigram added as third branch (Sprint 1 P0.4).
//
// Why hardcoded? RRF tuning has no user pressure for runtime changes.
// If future need arises, expose via MemoryStore::new + RwLock<f64>
// (mirror the fts_language pattern).
const RRF_K: f64 = 60.0;
const W_SEM: f64 = 0.6;   // было 0.7 до добавления trigram
const W_FTS: f64 = 0.25;  // было 0.3
const W_TRGM: f64 = 0.15; // новое
const TRGM_SIMILARITY_THRESHOLD: f32 = 0.3;  // pg_trgm default

// ── Store ────────────────────────────────────────────────────────────────────

pub struct MemoryStore {
    db: PgPool,
    embedder: Arc<dyn EmbeddingService>,
    /// `PostgreSQL` FTS dictionary (e.g. "russian", "english", "simple").
    /// Mutable at runtime via API.
    fts_language: RwLock<String>,
}

impl MemoryStore {
    /// Create a new `MemoryStore` with the given embedder and FTS language.
    pub fn new(db: PgPool, embedder: Arc<dyn EmbeddingService>, fts_language: String) -> Self {
        Self {
            db,
            embedder,
            fts_language: RwLock::new(fts_language),
        }
    }

    /// Test helper: create a store with a custom embedder.
    #[cfg(test)]
    pub fn test_with_embedder(embedder: Arc<dyn EmbeddingService>) -> Self {
        Self {
            db: PgPool::connect_lazy("postgres://invalid").unwrap(),
            embedder,
            fts_language: RwLock::new("simple".to_string()),
        }
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    /// Returns true when embedding is enabled and endpoint is configured.
    pub fn is_available(&self) -> bool {
        self.embedder.is_available()
    }

    /// Returns the current FTS language.
    pub fn fts_language(&self) -> String {
        self.fts_language.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Returns the FTS language after validating it is safe for SQL interpolation.
    /// regconfig cannot be parameterized, so we must validate before format!().
    /// Delegates to `crate::memory::admin::validated_fts_language` for the rule.
    pub fn validated_fts_language(&self) -> anyhow::Result<String> {
        crate::memory::admin::validated_fts_language(&self.fts_language())
    }

    /// Update the FTS language at runtime (normalizes to lowercase).
    pub fn set_fts_language(&self, lang: &str) {
        *self.fts_language.write().unwrap_or_else(std::sync::PoisonError::into_inner) = lang.to_ascii_lowercase();
    }

    /// Returns a reference to the embedder.
    pub fn embedder(&self) -> &Arc<dyn EmbeddingService> {
        &self.embedder
    }

    // ── Search ───────────────────────────────────────────────────────────────

    /// Search memory: hybrid (semantic + FTS via RRF) when embedding available, pure FTS fallback.
    /// Returns (results, `search_mode`) where `search_mode` is "hybrid", "semantic", or "fts".
    /// `exclude_ids`: chunk IDs already loaded via L0 pinned loading -- excluded from results (CTX-04).
    /// `agent_id`: filter results to this agent's chunks plus shared chunks. Pass `""` to search all.
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        exclude_ids: &[String],
        agent_id: &str,
    ) -> Result<(Vec<MemoryResult>, &'static str)> {
        if query.trim().is_empty() {
            return Ok((vec![], "none"));
        }

        let (mut results, mode) = if !self.is_available() || self.embedder.dim_mismatch() {
            let fts = self.search_fts(query, limit, agent_id).await?;
            let mode_str = if self.embedder.dim_mismatch() {
                "fts-degraded"
            } else {
                "fts"
            };
            (fts, mode_str)
        } else {
            // Run semantic + FTS in parallel and merge via RRF
            match self.search_hybrid(query, limit, agent_id).await {
                Ok(results) if !results.is_empty() => (results, "hybrid"),
                Ok(_) => {
                    let fts = self.search_fts(query, limit, agent_id).await?;
                    if fts.is_empty() {
                        // Last-resort fallback: AND-mode FTS returned nothing
                        // (multi-word queries where the document doesn't contain
                        // every word, e.g. "WSL Windows Subsystem Linux" against
                        // a doc that has WSL/Windows/Linux but not "Subsystem").
                        let lang = self.validated_fts_language()?;
                        let fts_or = crate::db::memory_queries::search_fts_or(
                            &self.db, query, limit as i64, &lang, agent_id,
                        ).await?;
                        (fts_or, "fts_or")
                    } else {
                        (fts, "fts")
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "hybrid search failed, falling back to FTS");
                    let fts = self.search_fts(query, limit, agent_id).await?;
                    (fts, "fts")
                }
            }
        };

        // L2 dedup: remove chunks already loaded via L0 pinned loading (CTX-04)
        if !exclude_ids.is_empty() {
            results.retain(|r| !exclude_ids.contains(&r.id));
        }

        Ok((results, mode))
    }

    /// Hybrid search: semantic + FTS merged via Reciprocal Rank Fusion (RRF).
    async fn search_hybrid(&self, query: &str, limit: usize, agent_id: &str) -> Result<Vec<MemoryResult>> {
        use std::collections::HashMap;

        let (sem_result, fts_result, trgm_result) = tokio::join!(
            self.search_semantic(query, limit * 2, agent_id),
            self.search_fts(query, limit * 2, agent_id),
            crate::db::memory_queries::search_trigram(
                &self.db, query, (limit * 2) as i64,
                TRGM_SIMILARITY_THRESHOLD, agent_id,
            ),
        );

        let sem = match sem_result {
            Ok(v) => v,
            Err(e) => { tracing::warn!(error = %e, "semantic search failed"); vec![] }
        };
        let fts = match fts_result {
            Ok(v) => v,
            Err(e) => { tracing::warn!(error = %e, "FTS search failed"); vec![] }
        };
        let trgm = match trgm_result {
            Ok(v) => v,
            Err(e) => { tracing::warn!(error = %e, "trigram search failed"); vec![] }
        };

        // Single-branch shortcut (если только одна ветка дала результаты — RRF не нужен)
        match (sem.is_empty(), fts.is_empty(), trgm.is_empty()) {
            (true, true, true) => return Ok(vec![]),
            (false, true, true) => return Ok(sem.into_iter().take(limit).collect()),
            (true, false, true) => return Ok(fts.into_iter().take(limit).collect()),
            (true, true, false) => return Ok(trgm.into_iter().take(limit).collect()),
            _ => {} // 2+ непустые → RRF
        }

        let sem_ranks: HashMap<String, usize> = sem.iter()
            .enumerate().map(|(i, r)| (r.id.clone(), i)).collect();
        let fts_ranks: HashMap<String, usize> = fts.iter()
            .enumerate().map(|(i, r)| (r.id.clone(), i)).collect();
        let trgm_ranks: HashMap<String, usize> = trgm.iter()
            .enumerate().map(|(i, r)| (r.id.clone(), i)).collect();

        let mut all: HashMap<String, MemoryResult> = HashMap::new();
        for r in sem { all.entry(r.id.clone()).or_insert(r); }
        for r in fts { all.entry(r.id.clone()).or_insert(r); }
        for r in trgm { all.entry(r.id.clone()).or_insert(r); }

        let mut scored: Vec<(f64, MemoryResult)> = all.into_values().map(|r| {
            let sem_rrf = sem_ranks.get(&r.id)
                .map_or(0.0, |&rank| 1.0 / (RRF_K + rank as f64 + 1.0));
            let fts_rrf = fts_ranks.get(&r.id)
                .map_or(0.0, |&rank| 1.0 / (RRF_K + rank as f64 + 1.0));
            let trgm_rrf = trgm_ranks.get(&r.id)
                .map_or(0.0, |&rank| 1.0 / (RRF_K + rank as f64 + 1.0));
            (W_SEM * sem_rrf + W_FTS * fts_rrf + W_TRGM * trgm_rrf, r)
        }).collect();

        // Score-descending; on ties, fall back to chunk id ascending for determinism.
        // Without the secondary key, ordering follows HashMap::into_values iteration
        // (random under default hasher) and integration tests flake.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        Ok(scored.into_iter().take(limit).map(|(_, r)| r).collect())
    }

    /// Semantic similarity search with MMR reranking (lambda=0.75).
    async fn search_semantic(&self, query: &str, limit: usize, agent_id: &str) -> Result<Vec<MemoryResult>> {
        let embedding = self.embedder.embed(query).await?;
        let vec_str = fmt_vec(&embedding);
        let candidate_limit = (limit * 6) as i64;

        let mut candidates = crate::db::memory_queries::search_semantic(
            &self.db, &vec_str, candidate_limit, agent_id,
        )
        .await?;

        // MMR reranking (lambda=0.75): balance relevance vs diversity.
        // Penalty = max inter-result similarity, approximated via min(candidate_sim, selected_sim)
        // since we only have query-similarity, not cross-embeddings.
        let mut results: Vec<MemoryResult> = Vec::with_capacity(limit);
        let mut selected_sims: Vec<f64> = Vec::with_capacity(limit);
        let lam = 0.75_f64;

        for _ in 0..limit.min(candidates.len()) {
            let mut best_idx = 0;
            let mut best_score = f64::NEG_INFINITY;

            for (i, c) in candidates.iter().enumerate() {
                let relevance = c.similarity * c.relevance_score;
                let max_sim_to_selected = selected_sims.iter()
                    .map(|&r_sim| c.similarity.min(r_sim))
                    .fold(0.0_f64, f64::max);
                let score = lam * relevance - (1.0 - lam) * max_sim_to_selected;
                if score > best_score {
                    best_score = score;
                    best_idx = i;
                }
            }
            let selected = candidates.remove(best_idx);
            selected_sims.push(selected.similarity);
            results.push(selected);
        }

        // Update accessed_at for returned chunks
        let ids: Vec<uuid::Uuid> = results.iter().filter_map(|r| r.id.parse().ok()).collect();
        crate::db::memory_queries::touch_accessed(&self.db, &ids).await;

        Ok(results)
    }

    /// Full-text search using `PostgreSQL` tsvector/tsquery with morphological stemming.
    /// Used as fallback when embedding endpoint is unavailable.
    /// `agent_id`: filter results to this agent's chunks plus shared chunks. Pass `""` to search all.
    pub async fn search_fts(&self, query: &str, limit: usize, agent_id: &str) -> Result<Vec<MemoryResult>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }

        let lang = self.validated_fts_language()?;

        let results = crate::db::memory_queries::search_fts(
            &self.db, query, limit as i64, &lang, agent_id,
        )
        .await?;

        // Update accessed_at
        let ids: Vec<uuid::Uuid> = results.iter().filter_map(|r| r.id.parse().ok()).collect();
        crate::db::memory_queries::touch_accessed(&self.db, &ids).await;

        Ok(results)
    }

    // ── Index ────────────────────────────────────────────────────────────────

    /// Generate embedding and insert a new memory chunk. Returns the new chunk UUID.
    /// `scope`: "private" (agent-only) or "shared" (visible to all agents).
    /// `agent_id`: the agent that owns this chunk (used for visibility filtering).
    pub async fn index(
        &self,
        content: &str,
        source: &str,
        pinned: bool,
        scope: &str,
        agent_id: &str,
    ) -> Result<String> {
        if self.embedder.dim_mismatch() {
            anyhow::bail!("dim_mismatch: reindex required (POST /api/memory/reindex)");
        }
        let lang = self.validated_fts_language()?;
        let embedding = self.embedder.embed(content).await?;
        let vec_str = fmt_vec(&embedding);
        let id = uuid::Uuid::new_v4().to_string();
        crate::db::memory_queries::insert_chunk(
            &self.db, &id, content, &vec_str, source, pinned, &lang, scope, agent_id,
        ).await?;
        Ok(id)
    }

    /// Re-index a source safely (F065): EMBED FIRST — the failure-prone step —
    /// and delete the source's existing chunks only AFTER embedding succeeds,
    /// then insert. So a transient embedding outage returns Err WITHOUT having
    /// dropped the old chunks, instead of the previous delete-then-index order
    /// that left the file with zero searchable chunks on any embedding blip.
    pub async fn reindex_source(
        &self,
        content: &str,
        source: &str,
        pinned: bool,
        scope: &str,
        agent_id: &str,
    ) -> Result<String> {
        if self.embedder.dim_mismatch() {
            anyhow::bail!("dim_mismatch: reindex required (POST /api/memory/reindex)");
        }
        let lang = self.validated_fts_language()?;
        // Embed BEFORE touching existing chunks — if this fails, the old chunks
        // stay intact and searchable.
        let embedding = self.embedder.embed(content).await?;
        let vec_str = fmt_vec(&embedding);
        // Embedding succeeded — now replace old chunks with the fresh one.
        self.delete_by_source(source).await?;
        let id = uuid::Uuid::new_v4().to_string();
        crate::db::memory_queries::insert_chunk(
            &self.db, &id, content, &vec_str, source, pinned, &lang, scope, agent_id,
        )
        .await?;
        Ok(id)
    }

    /// Batch index: embed multiple texts and insert them all. Returns chunk IDs.
    /// Tuple: (content, source, pinned, scope).
    /// `agent_id`: the agent that owns these chunks (used for visibility filtering).
    pub async fn index_batch(&self, items: &[(String, String, bool, String)], agent_id: &str) -> Result<Vec<String>> {
        if items.is_empty() {
            return Ok(vec![]);
        }

        if self.embedder.dim_mismatch() {
            anyhow::bail!("dim_mismatch: reindex required (POST /api/memory/reindex)");
        }

        let lang = self.validated_fts_language()?;
        let texts: Vec<&str> = items.iter().map(|(c, _, _, _)| c.as_str()).collect();
        let embeddings = self.embedder.embed_batch(&texts).await?;

        let mut tx = self.db.begin().await.context("failed to begin transaction for batch index")?;
        let mut ids = Vec::with_capacity(items.len());
        for (i, (content, source, pinned, scope)) in items.iter().enumerate() {
            let vec_str = fmt_vec(&embeddings[i]);
            let id = uuid::Uuid::new_v4().to_string();
            crate::db::memory_queries::insert_chunk_tx(
                &mut tx, &id, content, &vec_str, source, *pinned, &lang, scope, agent_id,
            ).await
            .context("failed to insert memory chunk in batch")?;
            ids.push(id);
        }
        tx.commit().await.context("failed to commit batch index")?;
        Ok(ids)
    }

    // ── Get ──────────────────────────────────────────────────────────────────

    /// Retrieve chunks by ID, by source, or most-recently-accessed (when both empty).
    pub async fn get(
        &self,
        chunk_id: Option<&str>,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryChunk>> {
        match (chunk_id, source) {
            (Some(id), _) => {
                crate::db::memory_queries::get_chunk_by_id(&self.db, id).await
            }
            (None, Some(src)) => {
                crate::db::memory_queries::get_chunks_by_source(&self.db, src, limit as i64).await
            }
            (None, None) => {
                crate::db::memory_queries::get_chunks_recent(&self.db, limit as i64).await
            }
        }
    }

    /// Return the most-recently-accessed memory chunks (pinned first).
    pub async fn recent(&self, limit: i64) -> Result<Vec<MemoryResult>> {
        crate::db::memory_queries::fetch_recent(&self.db, limit).await
    }

    // ── Load ─────────────────────────────────────────────────────────────────

    /// Load L0 pinned chunks for an agent, respecting token budget.
    /// Returns (formatted text for prompt, list of chunk IDs for L2 dedup).
    /// Budget is in tokens, approximated as `content.len()` / 4.
    /// When total exceeds budget, oldest chunks (FIFO) are dropped first.
    pub async fn load_pinned(
        &self,
        agent_id: &str,
        budget_tokens: u32,
    ) -> Result<(String, Vec<String>)> {
        let chunks = crate::db::memory_queries::fetch_pinned(&self.db, agent_id).await?;
        if chunks.is_empty() {
            return Ok((String::new(), vec![]));
        }

        // Calculate token estimates for all chunks (oldest first from SQL ORDER BY created_at ASC)
        let chunk_tokens: Vec<u32> = chunks.iter()
            .map(|c| (c.content.len() as u32) / 4)
            .collect();
        let total_tokens: u32 = chunk_tokens.iter().sum();

        // Determine how many oldest chunks to skip (FIFO drop: drop oldest first)
        let mut skip_count = 0usize;
        let mut remaining = total_tokens;
        if remaining > budget_tokens {
            for &ct in &chunk_tokens {
                if remaining <= budget_tokens {
                    break;
                }
                remaining -= ct;
                skip_count += 1;
            }
        }

        if skip_count > 0 {
            tracing::warn!(
                dropped = skip_count,
                budget = budget_tokens,
                total = total_tokens,
                "pinned chunks exceed token budget"
            );
        }

        let mut text_parts: Vec<String> = Vec::new();
        let mut ids: Vec<String> = Vec::new();

        for chunk in chunks.iter().skip(skip_count) {
            let source = if chunk.source.is_empty() { "memory" } else { &chunk.source };
            let content = crate::agent::pipeline::memory::truncate_chunk_content(&chunk.content);
            text_parts.push(format!("- [{}] {}", source, content));
            ids.push(chunk.id.clone());
        }

        let text = if text_parts.is_empty() {
            String::new()
        } else {
            format!("\n\n## Known Facts\n{}", text_parts.join("\n"))
        };

        Ok((text, ids))
    }

    // ── Delete / Admin ───────────────────────────────────────────────────────

    /// Delete a memory chunk by UUID. Returns true if a row was deleted.
    pub async fn delete(&self, chunk_id: &str) -> Result<bool> {
        crate::db::memory_queries::delete_chunk(&self.db, chunk_id).await
    }

    /// Delete all chunks with a given source (e.g. filename).
    pub async fn delete_by_source(&self, source: &str) -> Result<u64> {
        crate::db::memory_queries::delete_by_source(&self.db, source).await
    }

    /// Rebuild all tsv columns with the current FTS language.
    /// Called after changing `fts_language` to re-stem existing content.
    pub async fn rebuild_fts(&self) -> Result<u64> {
        let lang = self.validated_fts_language()?;
        let rows = crate::db::memory_queries::rebuild_fts(&self.db, &lang).await?;
        tracing::info!(lang = %lang, rows, "FTS index rebuilt");
        Ok(rows)
    }

    /// Wipe all memory for an agent.
    /// Returns the number of memory chunks deleted.
    ///
    /// Audit 2026-05-08 (7th pass): no longer called by the reindex flow —
    /// the memory-worker handles `clear_existing` atomically (see
    /// `reindex.rs`'s trailing DELETE gated on `created_at < reindex_started`),
    /// removing the prior race where Core wiped first and an enqueue
    /// failure left the agent empty. Retained for admin / future
    /// agent-deletion paths.
    #[allow(dead_code)]
    pub async fn wipe_agent_memory(&self, agent_id: &str) -> Result<u64> {
        crate::db::memory_queries::wipe_agent_memory(&self.db, agent_id).await
    }

    /// Insert a reindex task into the memory worker queue.
    pub async fn enqueue_reindex_task(&self, params: serde_json::Value) -> Result<uuid::Uuid> {
        crate::db::memory_queries::enqueue_reindex_task(&self.db, params).await
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedding::FakeEmbedder;

    #[tokio::test]
    async fn test_store_delegates_availability() {
        let store = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: true }));
        assert!(store.is_available());

        let store2 = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: false }));
        assert!(!store2.is_available());
    }

    #[tokio::test]
    async fn set_fts_language_normalizes_to_lowercase() {
        let store = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: false }));
        store.set_fts_language("Russian");
        assert_eq!(store.fts_language(), "russian");
    }

    #[tokio::test]
    async fn set_fts_language_stores_lowercase_ascii() {
        let store = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: false }));
        store.set_fts_language("ENGLISH");
        assert_eq!(store.fts_language(), "english");
    }

    #[tokio::test]
    async fn fts_language_returns_initial_value() {
        let store = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: false }));
        // test_with_embedder initializes to "simple"
        assert_eq!(store.fts_language(), "simple");
    }

    #[tokio::test]
    async fn validated_fts_language_accepts_valid_lang() {
        let store = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: false }));
        store.set_fts_language("russian");
        assert!(store.validated_fts_language().is_ok());
        assert_eq!(store.validated_fts_language().unwrap(), "russian");
    }

    #[tokio::test]
    async fn validated_fts_language_rejects_empty_lang_through_store() {
        // Tests the full MemoryStore::validated_fts_language() delegation path.
        // admin::validated_fts_language rejects: empty strings, uppercase, non-ASCII.
        let store = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: false }));
        *store.fts_language.write().unwrap() = String::new();
        assert!(store.validated_fts_language().is_err(), "store must reject empty lang");
    }

    #[tokio::test]
    async fn validated_fts_language_rejects_uppercase_through_store() {
        let store = MemoryStore::test_with_embedder(Arc::new(FakeEmbedder { available: false }));
        *store.fts_language.write().unwrap() = "Russian".to_string();
        assert!(store.validated_fts_language().is_err(), "store must reject uppercase lang");
    }

    use std::sync::atomic::{AtomicBool, Ordering};

    /// Embedder, который сообщает `dim_mismatch=true` через trait-обёртку.
    /// Используется для проверки guard-логики в MemoryStore без реальной БД.
    /// Двойная handle (raw + arc.clone) позволяет проверить `embed_calls`
    /// без unsafe-downcast'а.
    struct DimMismatchEmbedder {
        mismatch: AtomicBool,
        embed_calls: AtomicBool,
    }

    #[async_trait::async_trait]
    impl crate::memory::EmbeddingService for DimMismatchEmbedder {
        fn is_available(&self) -> bool { true }
        fn embed_dim(&self) -> u32 { 4 }
        fn embed_provider_display(&self) -> Option<String> { Some("fake".into()) }
        fn dim_mismatch(&self) -> bool { self.mismatch.load(Ordering::Acquire) }
        async fn embed(&self, _t: &str) -> anyhow::Result<Vec<f32>> {
            self.embed_calls.store(true, Ordering::Release);
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        async fn embed_batch(&self, _ts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.embed_calls.store(true, Ordering::Release);
            Ok(vec![vec![0.1, 0.2, 0.3, 0.4]])
        }
    }

    #[tokio::test]
    async fn index_returns_err_on_dim_mismatch() {
        let raw = std::sync::Arc::new(DimMismatchEmbedder {
            mismatch: AtomicBool::new(true),
            embed_calls: AtomicBool::new(false),
        });
        let embedder: std::sync::Arc<dyn crate::memory::EmbeddingService> = raw.clone();
        let store = MemoryStore::test_with_embedder(embedder);

        let res = store
            .index("text", "source", false, "private", "agent")
            .await;
        assert!(res.is_err());
        let err_msg = format!("{:#}", res.unwrap_err());
        assert!(err_msg.contains("dim_mismatch"), "got: {err_msg}");

        // embedder.embed НЕ должен был вызываться.
        assert!(
            !raw.embed_calls.load(Ordering::Acquire),
            "embed() should not be called when dim_mismatch=true"
        );
    }
}

// ── Hybrid-RRF integration tests ────────────────────────────────────────────
//
// 3-way RRF combining (semantic + FTS + trigram) needs a live database to
// verify, so this module is gated to Linux/x86_64 (testcontainers / Docker)
// and uses `#[sqlx::test]` per case for schema isolation. Previously lived in
// `tests/test_search_hybrid_rrf.rs` and reached `MemoryStore` via the
// `memory_test_facade` lib-bridge — moved inline as part of the lib.rs facade
// cleanup so the bridge can be deleted. The test bodies are byte-identical to
// the originals; only the module surface changed.
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod search_hybrid_rrf_tests {
    use super::MemoryStore;
    use crate::memory::embedding::EmbeddingService;
    use async_trait::async_trait;
    use sqlx::PgPool;
    use std::sync::Arc;

    // ── Fake embedders ──────────────────────────────────────────────────────

    /// Returns a fixed 4-dimensional vector for every input. The semantic
    /// branch of `search_hybrid` ranks by cosine distance — when every chunk
    /// has the same embedding, every chunk has identical similarity to the
    /// query, so the branch contributes a stable but un-discriminating
    /// ranking. That's exactly what we want here: the test asserts that the
    /// *combiner* runs and the *shortcut paths* return correctly, not that
    /// the embedding model is any good.
    ///
    /// Renamed from `FakeEmbedder` → `RrfFakeEmbedder` to avoid colliding
    /// with `embedding::FakeEmbedder` already in scope of the parent
    /// `tests` module.
    struct RrfFakeEmbedder;

    #[async_trait]
    impl EmbeddingService for RrfFakeEmbedder {
        fn is_available(&self) -> bool { true }
        fn embed_dim(&self) -> u32 { 4 }
        fn embed_provider_display(&self) -> Option<String> { Some("fake".to_string()) }
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.5, 0.5, 0.5, 0.5])
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok((0..texts.len()).map(|_| vec![0.5, 0.5, 0.5, 0.5]).collect())
        }
    }

    /// Discriminating embedder: maps anchor keywords to distinct unit
    /// vectors so the semantic branch produces a meaningful ranking.
    /// Without this, every chunk has identical cosine similarity to the
    /// query and the semantic branch contributes only positional noise to
    /// RRF — making it impossible to assert that the combiner actually
    /// fuses three independent rankings.
    struct KeywordEmbedder;

    #[async_trait]
    impl EmbeddingService for KeywordEmbedder {
        fn is_available(&self) -> bool { true }
        fn embed_dim(&self) -> u32 { 4 }
        fn embed_provider_display(&self) -> Option<String> { Some("keyword-fake".to_string()) }
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
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
            for t in texts { out.push(self.embed(t).await?); }
            Ok(out)
        }
    }

    /// Embedder that reports `is_available() == false`. Forces
    /// `MemoryStore::search` to take the FTS-only fallback branch — used to
    /// verify the combiner shortcut gating.
    struct DisabledEmbedder;

    #[async_trait]
    impl EmbeddingService for DisabledEmbedder {
        fn is_available(&self) -> bool { false }
        fn embed_dim(&self) -> u32 { 0 }
        fn embed_provider_display(&self) -> Option<String> { None }
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("embedding unavailable")
        }
        async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("embedding unavailable")
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    async fn insert_chunk_with_embedding(db: &PgPool, content: &str, agent_id: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO memory_chunks (id, content, source, pinned, scope, agent_id, embedding, tsv) \
             VALUES ($1::uuid, $2, 'test', false, 'private', $3, $4::vector, to_tsvector('russian', $2))",
        )
        .bind(&id).bind(content).bind(agent_id).bind("[0.5,0.5,0.5,0.5]")
        .execute(db).await.expect("insert chunk with embedding");
        id
    }

    async fn insert_chunk_with_vec(
        db: &PgPool, content: &str, agent_id: &str, embedding: [f32; 4],
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
        .bind(&id).bind(content).bind(agent_id).bind(&vec_str)
        .execute(db).await.expect("insert chunk with custom embedding");
        id
    }

    async fn insert_chunk_no_embedding(db: &PgPool, content: &str, agent_id: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO memory_chunks (id, content, source, pinned, scope, agent_id, tsv) \
             VALUES ($1::uuid, $2, 'test', false, 'private', $3, to_tsvector('russian', $2))",
        )
        .bind(&id).bind(content).bind(agent_id)
        .execute(db).await.expect("insert chunk without embedding");
        id
    }

    // ── Tests ───────────────────────────────────────────────────────────────

    /// Fires the full RRF combiner: every branch returns at least one chunk,
    /// so the 8-state shortcut falls through to the actual rank-fusion code path.
    #[sqlx::test(migrations = "../../migrations")]
    async fn search_hybrid_returns_results_when_all_three_branches_match(db: PgPool) {
        let agent = format!("test-rrf-all-{}", uuid::Uuid::new_v4());
        insert_chunk_with_embedding(&db, "RRF_TEST_пользователь данные", &agent).await;
        insert_chunk_with_embedding(&db, "RRF_TEST_пользователи система", &agent).await;
        insert_chunk_with_embedding(&db, "RRF_TEST_пользоват_partial_match", &agent).await;

        let store = MemoryStore::new(db.clone(), Arc::new(RrfFakeEmbedder), "russian".to_string());
        let (results, mode) = store.search("пользоват", 10, &[], &agent).await.expect("search");

        assert_eq!(mode, "hybrid", "expected hybrid mode when every branch matches, got {mode}");
        assert!(!results.is_empty(), "RRF combiner must return at least one result");
        let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
        assert!(
            contents.iter().any(|c| c.contains("RRF_TEST_")),
            "results should include the test chunks, got: {contents:?}"
        );
        sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
            .bind(&agent).execute(&db).await.ok();
    }

    /// Empty query short-circuits the entire pipeline before any branch runs.
    #[sqlx::test(migrations = "../../migrations")]
    async fn search_hybrid_empty_query_returns_empty(db: PgPool) {
        let agent = format!("test-rrf-empty-{}", uuid::Uuid::new_v4());
        insert_chunk_with_embedding(&db, "RRF_EMPTY_data", &agent).await;
        let store = MemoryStore::new(db.clone(), Arc::new(RrfFakeEmbedder), "russian".to_string());
        let (results, mode) = store.search("", 5, &[], &agent).await.expect("search empty");
        assert!(results.is_empty(), "empty query must return no results");
        assert_eq!(mode, "none", "empty query must report mode='none'");
    }

    /// Trigram-only path: chunk has no embedding (semantic skips it) and the
    /// query is a typo. Trigram fuzzy match is the only branch that fires.
    #[sqlx::test(migrations = "../../migrations")]
    async fn search_hybrid_returns_results_for_typo_recovery(db: PgPool) {
        let agent = format!("test-rrf-typo-{}", uuid::Uuid::new_v4());
        insert_chunk_no_embedding(&db, "RRF_TYPO_пользоветель", &agent).await;
        let store = MemoryStore::new(db.clone(), Arc::new(RrfFakeEmbedder), "russian".to_string());
        let (results, _mode) = store.search("пользователь", 5, &[], &agent).await.expect("search typo");
        let contents: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
        assert!(
            contents.iter().any(|c| c.contains("пользоветель")),
            "trigram branch must surface the typo'd chunk, got: {contents:?}"
        );
        sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
            .bind(&agent).execute(&db).await.ok();
    }

    /// RRF fusion math: a chunk that ranks in 2 of 3 layers must outrank a
    /// chunk that ranks only in 1 layer.
    #[sqlx::test(migrations = "../../migrations")]
    async fn search_hybrid_rewards_multi_layer_chunks(db: PgPool) {
        let agent = format!("test-rrf-fusion-{}", uuid::Uuid::new_v4());
        let sem_only = insert_chunk_with_vec(&db, "qwertyuiop_xyz_marker_unique", &agent, [1.0, 0.0, 0.0, 0.0]).await;
        let fts_only = insert_chunk_with_vec(&db, "контекст другое значение", &agent, [0.0, 1.0, 0.0, 0.0]).await;
        let multi = insert_chunk_with_vec(&db, "контекст система winner_chunk", &agent, [1.0, 0.0, 0.0, 0.0]).await;

        let store = MemoryStore::new(db.clone(), Arc::new(KeywordEmbedder), "russian".to_string());
        let (results, mode) = store.search("RRF_ALPHA контекст система", 5, &[], &agent).await.expect("hybrid search");

        assert_eq!(mode, "hybrid", "all branches non-empty must pick hybrid mode");
        assert!(!results.is_empty(), "expected at least one result, got {}", results.len());
        let top_id = &results[0].id;
        assert_eq!(
            top_id, &multi,
            "multi-layer chunk must rank #1 over single-layer chunks (RRF math broken?)\n\
             Top: {top_id}\nMulti: {multi}\nSemOnly: {sem_only}\nFtsOnly: {fts_only}"
        );
        sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
            .bind(&agent).execute(&db).await.ok();
    }

    /// Determinism guard: HashMap ordering is non-deterministic without an
    /// explicit secondary sort key. Five runs must yield identical top-N.
    #[sqlx::test(migrations = "../../migrations")]
    async fn search_hybrid_results_are_deterministic_under_ties(db: PgPool) {
        let agent = format!("test-rrf-det-{}", uuid::Uuid::new_v4());
        insert_chunk_with_vec(&db, "система данные", &agent, [1.0, 0.0, 0.0, 0.0]).await;
        insert_chunk_with_vec(&db, "система данные", &agent, [0.0, 1.0, 0.0, 0.0]).await;
        insert_chunk_with_vec(&db, "система данные", &agent, [0.0, 0.0, 1.0, 0.0]).await;

        let store = MemoryStore::new(db.clone(), Arc::new(KeywordEmbedder), "russian".to_string());
        let mut runs: Vec<Vec<String>> = Vec::with_capacity(5);
        for _ in 0..5 {
            let (results, _mode) = store.search("система данные", 5, &[], &agent).await.expect("hybrid search");
            runs.push(results.iter().map(|r| r.id.clone()).collect());
        }
        let first = &runs[0];
        for (i, r) in runs.iter().enumerate() {
            assert_eq!(r, first, "run {i} ordering diverged from run 0 — RRF tie-break is non-deterministic");
        }
        sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
            .bind(&agent).execute(&db).await.ok();
    }

    /// Disabled embedder skips the hybrid combiner and falls through to FTS.
    #[sqlx::test(migrations = "../../migrations")]
    async fn search_hybrid_skipped_when_embedder_unavailable(db: PgPool) {
        let agent = format!("test-rrf-no-embed-{}", uuid::Uuid::new_v4());
        insert_chunk_with_embedding(&db, "RRF NOEMBED данные системы", &agent).await;
        let store = MemoryStore::new(db.clone(), Arc::new(DisabledEmbedder), "russian".to_string());
        let (results, mode) = store.search("данные", 5, &[], &agent).await.expect("search no-embed");
        assert_eq!(mode, "fts", "disabled embedder must force FTS-only mode, got {mode}");
        assert!(!results.is_empty(), "FTS branch alone must surface the matching chunk");
        sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
            .bind(&agent).execute(&db).await.ok();
    }
}
