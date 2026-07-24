/// Embedding service abstraction and Toolgate-based implementation.
///
/// `EmbeddingService` defines the interface for generating vector embeddings.
/// `ToolgateEmbedder` implements it by delegating HTTP calls to `opex_embedding::ToolgateClient`.
use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use opex_db::sys_flags;
use opex_embedding::ToolgateClient;
use serde_json::json;
use tokio::sync::Mutex;

// ── Trait ────────────────────────────────────────────────────────────────────

/// Abstraction over embedding generation.
///
/// Implementations must be `Send + Sync` for use behind `Arc`.
#[async_trait]
pub trait EmbeddingService: Send + Sync {
    /// Returns true when the embedding endpoint is configured and reachable.
    fn is_available(&self) -> bool;

    /// Returns the detected embedding dimension (0 if not yet detected).
    fn embed_dim(&self) -> u32;

    /// Returns the display-name of the active embedding provider (UI/logs only,
    /// never sent to embedding API). Renamed from `embed_model_name()` — old
    /// name was misleading, value was never a model id.
    fn embed_provider_display(&self) -> Option<String>;

    /// Generate an embedding vector for a single text.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Batch-embed multiple texts in a single request.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// P0.1: гейт для index/search-операций. Default `false` для embedders,
    /// которым понятие dim-mismatch неприменимо (FakeEmbedder, CountingEmbedder).
    /// ToolgateEmbedder override'ит это методом, читающим in-memory флаг.
    fn dim_mismatch(&self) -> bool {
        false
    }

    /// Сбросить dim_mismatch flag (вызывается из `POST /api/memory/reindex`).
    /// No-op для embedders без persistent state.
    async fn clear_dim_mismatch(&self) -> Result<()> {
        Ok(())
    }

    /// Non-blocking reset of in-memory state + persisted flags.
    /// Called from PUT /api/provider-active and DELETE /api/providers/{id}
    /// when the active embedding provider changes. No-op default for embedders
    /// without persistent state.
    async fn reset(&self) -> Result<()> {
        Ok(())
    }

    /// Sync persistent state (e.g. `system_flags["memory.dim_mismatch"]`) into
    /// in-memory atomics and run any one-time probes. No-op default.
    ///
    /// **Must be called at startup before serving HTTP** — without it the
    /// P0.1 dim_mismatch guard can be bypassed because the in-memory
    /// `AtomicBool` defaults to `false` until the first `embed()` call runs
    /// `do_initialize()`. `ToolgateEmbedder` overrides this to load the
    /// persistent flag and probe the embedding dimension eagerly.
    async fn ensure_initialized(&self) {}
}

// ── ToolgateEmbedder ─────────────────────────────────────────────────────────

/// Concrete embedder that delegates to `opex_embedding::ToolgateClient`.
///
/// Uses a generation-counter pattern so `reset()` is non-blocking:
/// - `init_generation` is bumped on every `reset()`.
/// - `initialized_generation` tracks last successful init.
/// - `ensure_initialized()` reruns init when the two diverge.
///
/// On dimension mismatch the `dim_mismatch` flag is raised — index/search
/// operations should check it and refuse to use semantic search until a
/// reindex completes (no destructive `clear_embeddings`).
pub struct ToolgateEmbedder {
    db: sqlx::PgPool,
    client: ToolgateClient,
    /// 0 = не детектирован.
    embed_dim: AtomicU32,
    /// Display-name провайдера (для UI/логов). НЕ передаётся в API.
    provider_display: ArcSwap<Option<String>>,
    /// Generation-counter: инкрементируется в `reset()`; `ensure_initialized()`
    /// сравнивает с `initialized_generation` и перезапускает init, если разошлись.
    init_generation: AtomicU64,
    initialized_generation: AtomicU64,
    init_mutex: Mutex<()>,
    /// Index/search-операции проверяют этот флаг.
    dim_mismatch: AtomicBool,
}

impl ToolgateEmbedder {
    /// Create a new embedder.
    ///
    /// `toolgate_url`: пустая строка → embedder в non-configured режиме (FTS-only).
    /// `embed_dim_hint`: 0 = auto-detect via probe.
    /// `requested_dimensions`: 0 = use model default.
    pub fn new(
        db: sqlx::PgPool,
        toolgate_url: &str,
        embed_dim_hint: u32,
        requested_dimensions: u32,
    ) -> Self {
        let client = ToolgateClient::new(toolgate_url.to_string(), requested_dimensions);
        Self {
            db,
            client,
            embed_dim: AtomicU32::new(embed_dim_hint),
            provider_display: ArcSwap::from_pointee(None),
            init_generation: AtomicU64::new(1),
            initialized_generation: AtomicU64::new(0),
            init_mutex: Mutex::new(()),
            dim_mismatch: AtomicBool::new(false),
        }
    }

    /// Create a disabled embedder (no URL). Used in tests when no embedding is needed.
    #[cfg(test)]
    pub fn new_disabled() -> Self {
        Self::new(
            sqlx::PgPool::connect_lazy("postgres://invalid").unwrap(),
            "",
            0,
            0,
        )
    }

    /// Initialize embedding: auto-detect dimension, validate DB, ensure HNSW index.
    /// Graceful: if embedding endpoint is unreachable, logs a warning and continues
    /// (FTS fallback will be used for search).
    ///
    /// P0.1: НЕ удаляет `memory_chunks` при dim mismatch. Ставит `dim_mismatch=true`
    /// и `system_flags["memory.dim_mismatch"]=true`. Reindex run by operator.
    async fn do_initialize(&self) -> Result<()> {
        // Sync persistent dim_mismatch flag → in-memory state.
        // Without this, after restart `dim_mismatch()` would lie about the
        // state until do_initialize completes, allowing index() calls to
        // bypass the P0.1 guard.
        if let Some(v) = sys_flags::get(&self.db, "memory.dim_mismatch").await
            && v.as_bool() == Some(true)
        {
            self.dim_mismatch.store(true, Ordering::Release);
        }

        if !self.client.is_configured() {
            tracing::info!("embedding not configured, memory will use FTS only");
            return Ok(());
        }

        // 1. embed_dim: hint > cached system_flags > probe.
        let current_dim = self.embed_dim.load(Ordering::Relaxed);
        let dim = if current_dim > 0 {
            current_dim
        } else if let Some(cached) = sys_flags::get(&self.db, "memory.embed_dim").await
            && let Some(d) = cached.as_u64()
        {
            self.embed_dim.store(d as u32, Ordering::Release);
            d as u32
        } else {
            match self.client.probe_dim().await {
                Ok(d) => {
                    self.embed_dim.store(d, Ordering::Release);
                    let _ = sys_flags::upsert(&self.db, "memory.embed_dim", json!(d)).await;
                    d
                }
                Err(e) => {
                    tracing::warn!(error = %e, "embedding probe failed at startup, degraded to FTS");
                    return Ok(());
                }
            }
        };

        // 2. Discover provider display-name via /health (best-effort).
        if let Ok(h) = self.client.fetch_health().await {
            tracing::info!(provider = ?h.active_embedding_provider, "discovered embedding provider");
            self.provider_display
                .store(Arc::new(h.active_embedding_provider));
        }

        // 3. Compare against existing memory_chunks dimension.
        //    P0.1: НЕ удаляем чанки. Ставим dim_mismatch=true и выходим.
        let existing_dim = opex_db::memory_queries::get_existing_embedding_dim(&self.db).await;
        if let Some(old_dim) = existing_dim
            && old_dim as u32 != dim
        {
            self.dim_mismatch.store(true, Ordering::Release);
            sys_flags::upsert(&self.db, "memory.dim_mismatch", json!(true)).await?;
            tracing::error!(
                old_dim,
                new_dim = dim,
                "embed dimension mismatch — semantic search disabled, run POST /api/memory/reindex"
            );
            return Ok(());
        }

        // 4. Ensure index (non-fatal — sequential scan works).
        if let Err(e) = opex_db::memory_queries::ensure_vector_index(&self.db, dim).await {
            tracing::info!(dim, error = %e, "vector index not created — using sequential scan");
        }

        tracing::info!(
            dim,
            provider = ?self.provider_display.load().as_ref().as_ref(),
            "embedding initialized"
        );
        Ok(())
    }

    /// Lazy init helper: если `embed_dim==0`, и пришёл вектор, выставляем dim
    /// и пытаемся создать HNSW индекс.
    async fn maybe_lazy_init_dim(&self, vec: &[f32]) {
        let detected_dim = vec.len() as u32;
        if self
            .embed_dim
            .compare_exchange(0, detected_dim, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            tracing::info!(
                dim = detected_dim,
                "embedding came online, lazy-initializing"
            );
            let _ = sys_flags::upsert(&self.db, "memory.embed_dim", json!(detected_dim)).await;
            if let Err(e) =
                opex_db::memory_queries::ensure_vector_index(&self.db, detected_dim).await
            {
                tracing::warn!(error = %e, "failed to create HNSW index during lazy init");
            }
        }
    }
}

#[async_trait]
impl EmbeddingService for ToolgateEmbedder {
    fn is_available(&self) -> bool {
        self.client.is_configured()
    }

    fn embed_dim(&self) -> u32 {
        self.embed_dim.load(Ordering::Relaxed)
    }

    fn embed_provider_display(&self) -> Option<String> {
        self.provider_display.load().as_ref().clone()
    }

    fn dim_mismatch(&self) -> bool {
        self.dim_mismatch.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Сбросить ТОЛЬКО `dim_mismatch` flag (после успешного reindex).
    async fn clear_dim_mismatch(&self) -> Result<()> {
        self.dim_mismatch.store(false, Ordering::Release);
        sys_flags::upsert(&self.db, "memory.dim_mismatch", json!(false)).await?;
        Ok(())
    }

    /// Non-blocking reset of all in-memory state + persisted flags.
    /// Bumps `init_generation`, clears `embed_dim`, `provider_display`,
    /// `dim_mismatch`, and deletes the persisted `memory.embed_dim` /
    /// `memory.dim_mismatch` keys.
    ///
    /// Не дожидается завершения in-flight probe — generation-counter гарантирует,
    /// что следующий `ensure_initialized()` пере-инициализирует embedder.
    ///
    /// **Warning:** clearing `dim_mismatch` here erases the "reindex required"
    /// signal. Call after PUT /api/provider-active (provider switch invalidates
    /// the old state anyway) — do NOT call as a generic "reset" without
    /// follow-up reindex, otherwise old chunks may be silently used with new
    /// embedding model.
    async fn reset(&self) -> Result<()> {
        self.init_generation.fetch_add(1, Ordering::AcqRel);
        self.embed_dim.store(0, Ordering::Release);
        self.provider_display.store(Arc::new(None));
        self.dim_mismatch.store(false, Ordering::Release);
        sys_flags::delete(&self.db, "memory.embed_dim").await?;
        sys_flags::upsert(&self.db, "memory.dim_mismatch", json!(false)).await?;
        Ok(())
    }

    /// Lazy initialization: runs embedding probe on first memory operation
    /// (or eagerly at startup from `main.rs`). Uses generation-counter — after
    /// `reset()` reruns init.
    ///
    /// **P0.1 critical:** must be called at startup before serving HTTP,
    /// otherwise `dim_mismatch()` lies about the persistent flag until the
    /// first embed call runs `do_initialize()`.
    async fn ensure_initialized(&self) {
        let target = self.init_generation.load(Ordering::Acquire);
        if self.initialized_generation.load(Ordering::Acquire) >= target {
            return; // fast path
        }
        let _guard = self.init_mutex.lock().await;
        let target = self.init_generation.load(Ordering::Acquire);
        if self.initialized_generation.load(Ordering::Acquire) >= target {
            return; // другой task завершил init пока ждали lock
        }
        if let Err(e) = self.do_initialize().await {
            tracing::warn!(error = %e, "embedding init failed — memory uses FTS only");
        }
        self.initialized_generation.store(target, Ordering::Release);
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.ensure_initialized().await;
        let vec = self.client.embed_one(text).await?;
        self.maybe_lazy_init_dim(&vec).await;
        let expected = self.embed_dim.load(Ordering::Relaxed);
        if expected > 0 && vec.len() as u32 != expected {
            anyhow::bail!(
                "embedding dimension mismatch: expected {}, got {}",
                expected,
                vec.len()
            );
        }
        Ok(vec)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        self.ensure_initialized().await;
        let results = self.client.embed_batch(texts).await?;
        if let Some(first) = results.first() {
            self.maybe_lazy_init_dim(first).await;
        }
        let expected = self.embed_dim.load(Ordering::Relaxed);
        if expected > 0 {
            for (i, v) in results.iter().enumerate() {
                if v.len() as u32 != expected {
                    anyhow::bail!(
                        "batch embedding dimension mismatch at index {}: expected {}, got {}",
                        i,
                        expected,
                        v.len()
                    );
                }
            }
        }
        Ok(results)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Format a float vector as a pgvector literal: "[0.1,0.2,...]"
pub fn fmt_vec(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 10 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

// ── Test-only embedders ──────────────────────────────────────────────────────

#[cfg(test)]
pub struct FakeEmbedder {
    pub available: bool,
}

#[cfg(test)]
#[async_trait]
impl EmbeddingService for FakeEmbedder {
    fn is_available(&self) -> bool {
        self.available
    }

    fn embed_dim(&self) -> u32 {
        if self.available { 4 } else { 0 }
    }

    fn embed_provider_display(&self) -> Option<String> {
        if self.available { Some("fake-model".to_string()) } else { None }
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        if self.available {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        } else {
            anyhow::bail!("embedding unavailable")
        }
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for t in texts {
            results.push(self.embed(t).await?);
        }
        Ok(results)
    }
}

/// Counting embedder: tracks number of `embed()` invocations. Used by
/// integration tests to verify caching / call de-duplication behaviour.
#[cfg(test)]
pub struct CountingEmbedder {
    pub calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl CountingEmbedder {
    pub fn new() -> Self {
        Self { calls: std::sync::atomic::AtomicUsize::new(0) }
    }

    pub fn count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
impl Default for CountingEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[async_trait]
impl EmbeddingService for CountingEmbedder {
    fn is_available(&self) -> bool { true }
    fn embed_dim(&self) -> u32 { 4 }
    fn embed_provider_display(&self) -> Option<String> { Some("counting".into()) }
    async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
    async fn embed_batch(&self, ts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(ts.len());
        for _ in ts { out.push(self.embed("").await?); }
        Ok(out)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_vec_empty() {
        assert_eq!(fmt_vec(&[]), "[]");
    }

    #[test]
    fn fmt_vec_multiple() {
        assert_eq!(fmt_vec(&[1.0, 2.5, -3.0]), "[1,2.5,-3]");
    }

    #[test]
    fn fmt_vec_no_spaces() {
        // pgvector literal must have no spaces between values
        let result = fmt_vec(&[0.1, 0.2, 0.3]);
        assert!(!result.contains(' '), "fmt_vec output must not contain spaces: {result}");
    }

    #[tokio::test]
    async fn test_unavailable_when_no_url() {
        let embedder = ToolgateEmbedder::new_disabled();
        assert!(!embedder.is_available());
    }
}
