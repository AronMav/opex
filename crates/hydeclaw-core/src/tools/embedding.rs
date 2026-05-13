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
    pub async fn get_or_embed(
        &self,
        key: &str,
        text: &str,
        embedder: &dyn crate::memory::EmbeddingService,
    ) -> anyhow::Result<Vec<f32>> {
        // get() двигает запись в начало LRU — нужен write-lock.
        if let Some(v) = self.embeddings.lock().await.get(key) {
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
