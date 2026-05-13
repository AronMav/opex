//! In-memory LRU cache for tool embeddings used by semantic top-K selection.
//!
//! Tool descriptors (name + description) are embedded once per unique key
//! and cached up to `CACHE_CAPACITY` entries with least-recently-used eviction.
//! The cache is shared across all calls within one agent engine instance.

use std::num::NonZeroUsize;

use lru::LruCache;
use tokio::sync::Mutex;

const CACHE_CAPACITY: usize = 200;

pub struct ToolEmbeddingCache {
    embeddings: Mutex<LruCache<String, Vec<f32>>>,
}

impl ToolEmbeddingCache {
    pub fn new() -> Self {
        Self {
            embeddings: Mutex::new(LruCache::new(
                NonZeroUsize::new(CACHE_CAPACITY).expect("capacity > 0"),
            )),
        }
    }

    /// Return the cached embedding for `key`, or compute it from `text` and cache it.
    ///
    /// **Self-healing on dim change:** if the cached vector length differs
    /// from the embedder's current `embed_dim()`, treat as a cache miss and
    /// re-embed. This prevents mixed-dim cosine similarities after the
    /// active embedding provider is switched (`embedder.reset()` clears the
    /// embedder's in-memory state, but does NOT reach into this per-agent LRU).
    pub async fn get_or_embed(
        &self,
        key: &str,
        text: &str,
        embedder: &dyn crate::memory::EmbeddingService,
    ) -> anyhow::Result<Vec<f32>> {
        let expected_dim = embedder.embed_dim() as usize;
        // get() двигает запись в начало LRU — нужен write-lock.
        // Dim-mismatch falls through (re-embed) to avoid mixed-dim cosine
        // similarity after the active embedding provider is switched.
        if let Some(v) = self.embeddings.lock().await.get(key)
            && (expected_dim == 0 || v.len() == expected_dim)
        {
            return Ok(v.clone());
        }
        let v = embedder.embed(text).await?;
        self.embeddings
            .lock()
            .await
            .put(key.to_string(), v.clone());
        Ok(v)
    }
}

/// Cosine similarity between two equal-length float vectors.
/// Returns 0.0 on zero-norm inputs.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors() {
        let v = [1.0f32, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn opposite_vectors() {
        let a = [-1.0f32, -2.0, -3.0];
        let b = [1.0f32, 2.0, 3.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors() {
        let a = [1.0f32, 0.0];
        let b = [0.0f32, 1.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn zero_norm_returns_zero() {
        let zero = [0.0f32, 0.0];
        let other = [1.0f32, 2.0];
        assert_eq!(cosine_similarity(&zero, &other), 0.0);
        assert_eq!(cosine_similarity(&other, &zero), 0.0);
    }

    #[test]
    fn single_element() {
        let a = [3.0f32];
        let b = [3.0f32];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    use crate::memory::embedding::CountingEmbedder;
    use crate::memory::EmbeddingService;

    /// Test-only embedder с настраиваемой dim — используется в
    /// `dim_change_invalidates_cached_vector`, чтобы смоделировать смену
    /// embedding-провайдера, при которой меняется размерность вектора.
    struct FixedDimEmbedder {
        dim: u32,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FixedDimEmbedder {
        fn new(dim: u32) -> Self {
            Self { dim, calls: std::sync::atomic::AtomicUsize::new(0) }
        }
        fn count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingService for FixedDimEmbedder {
        fn is_available(&self) -> bool { true }
        fn embed_dim(&self) -> u32 { self.dim }
        fn embed_provider_display(&self) -> Option<String> { Some("fixed-dim".into()) }
        async fn embed(&self, _t: &str) -> anyhow::Result<Vec<f32>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![0.1; self.dim as usize])
        }
        async fn embed_batch(&self, ts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(ts.len());
            for _ in ts { out.push(self.embed("").await?); }
            Ok(out)
        }
    }

    #[tokio::test]
    async fn dim_change_invalidates_cached_vector() {
        // Embedder #1: dim=4. Кэшируем вектор для ключа "tool".
        let embedder_v1 = FixedDimEmbedder::new(4);
        let cache = ToolEmbeddingCache::new();
        let v1 = cache.get_or_embed("tool", "tool", &embedder_v1).await.unwrap();
        assert_eq!(v1.len(), 4);
        assert_eq!(embedder_v1.count(), 1, "first call must miss + embed");

        // Повторный вызов с тем же embedder — cache hit.
        let _ = cache.get_or_embed("tool", "tool", &embedder_v1).await.unwrap();
        assert_eq!(embedder_v1.count(), 1, "second call must hit cache");

        // Переключились на embedder #2 с dim=8 (симулируем смену провайдера +
        // reset()). Cached dim=4 ≠ expected dim=8 → cache miss → re-embed.
        let embedder_v2 = FixedDimEmbedder::new(8);
        let v2 = cache.get_or_embed("tool", "tool", &embedder_v2).await.unwrap();
        assert_eq!(v2.len(), 8, "must return new-dim vector");
        assert_eq!(embedder_v2.count(), 1, "dim-change must force re-embed");

        // Следующий вызов с тем же v2 — снова cache hit (свежий 8-dim вектор).
        let _ = cache.get_or_embed("tool", "tool", &embedder_v2).await.unwrap();
        assert_eq!(embedder_v2.count(), 1, "subsequent call hits the refreshed cache");
    }

    #[tokio::test]
    async fn lru_evicts_least_recently_used_not_all() {
        let cache = ToolEmbeddingCache::new();
        let embedder = CountingEmbedder::new();

        // Заполнить кэш до cap (200 elements).
        for i in 0..200 {
            let key = format!("tool_{i}");
            cache.get_or_embed(&key, &key, &embedder).await.unwrap();
        }
        assert_eq!(embedder.count(), 200);

        // Touch tool_0 → перемещается в head LRU.
        cache.get_or_embed("tool_0", "tool_0", &embedder).await.unwrap();
        assert_eq!(embedder.count(), 200, "tool_0 must be a cache hit");

        // Вставить 201-й → эвикт самого старого (tool_1, поскольку tool_0 только что touched).
        cache.get_or_embed("tool_200", "tool_200", &embedder).await.unwrap();
        assert_eq!(embedder.count(), 201);

        // tool_0 должен ОСТАТЬСЯ в кэше (свежий) — повторный вызов = cache hit.
        cache.get_or_embed("tool_0", "tool_0", &embedder).await.unwrap();
        assert_eq!(embedder.count(), 201, "tool_0 must still be cached after touch");

        // tool_1 должен быть ЭВИКТНУТ — повторный вызов = cache miss.
        cache.get_or_embed("tool_1", "tool_1", &embedder).await.unwrap();
        assert_eq!(embedder.count(), 202, "tool_1 must have been evicted");
    }
}
