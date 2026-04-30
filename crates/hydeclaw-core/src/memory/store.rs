/// `MemoryStore` — the main entry point for memory operations (search, index, get, delete).
///
/// Delegates embedding to an `Arc<dyn EmbeddingService>` and executes queries
/// against PostgreSQL via `crate::db::memory_queries`.
use anyhow::{Context, Result};
use sqlx::PgPool;
use std::sync::{Arc, RwLock};

use super::embedding::{fmt_vec, EmbeddingService};
use super::{MemoryChunk, MemoryResult};

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

        let (mut results, mode) = if self.is_available() {
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
        } else {
            let fts = self.search_fts(query, limit, agent_id).await?;
            (fts, "fts")
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

        let (sem_result, fts_result) = tokio::join!(
            self.search_semantic(query, limit * 2, agent_id),
            self.search_fts(query, limit * 2, agent_id),
        );

        let sem = match sem_result {
            Ok(v) => v,
            Err(e) => { tracing::warn!(error = %e, "semantic search failed"); vec![] }
        };
        let fts = match fts_result {
            Ok(v) => v,
            Err(e) => { tracing::warn!(error = %e, "FTS search failed"); vec![] }
        };

        if sem.is_empty() { return Ok(fts.into_iter().take(limit).collect()); }
        if fts.is_empty() { return Ok(sem.into_iter().take(limit).collect()); }

        const K: f64 = 60.0;

        // Build rank maps for RRF scoring
        let sem_ranks: HashMap<String, usize> = sem.iter()
            .enumerate().map(|(i, r)| (r.id.clone(), i)).collect();
        let fts_ranks: HashMap<String, usize> = fts.iter()
            .enumerate().map(|(i, r)| (r.id.clone(), i)).collect();

        // Collect all unique results (semantic takes priority for the stored copy)
        let mut all: HashMap<String, MemoryResult> = HashMap::new();
        for r in sem { all.entry(r.id.clone()).or_insert(r); }
        for r in fts { all.entry(r.id.clone()).or_insert(r); }

        // Weighted RRF: semantic results get higher weight (0.7) than FTS (0.3)
        // to prevent noisy short-word FTS matches from displacing semantically relevant results.
        const W_SEM: f64 = 0.7;
        const W_FTS: f64 = 0.3;
        let mut scored: Vec<(f64, MemoryResult)> = all.into_values().map(|r| {
            let sem_rrf = sem_ranks.get(&r.id)
                .map_or(0.0, |&rank| 1.0 / (K + rank as f64 + 1.0));
            let fts_rrf = fts_ranks.get(&r.id)
                .map_or(0.0, |&rank| 1.0 / (K + rank as f64 + 1.0));
            (W_SEM * sem_rrf + W_FTS * fts_rrf, r)
        }).collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
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
        let lang = self.validated_fts_language()?;
        let embedding = self.embedder.embed(content).await?;
        let vec_str = fmt_vec(&embedding);
        let id = uuid::Uuid::new_v4().to_string();
        crate::db::memory_queries::insert_chunk(
            &self.db, &id, content, &vec_str, source, pinned, &lang, scope, agent_id,
        ).await?;
        Ok(id)
    }

    /// Batch index: embed multiple texts and insert them all. Returns chunk IDs.
    /// Tuple: (content, source, pinned, scope).
    /// `agent_id`: the agent that owns these chunks (used for visibility filtering).
    pub async fn index_batch(&self, items: &[(String, String, bool, String)], agent_id: &str) -> Result<Vec<String>> {
        if items.is_empty() {
            return Ok(vec![]);
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

}
