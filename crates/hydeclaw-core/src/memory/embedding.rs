/// Embedding service abstraction and Toolgate-based implementation.
///
/// `EmbeddingService` defines the interface for generating vector embeddings.
/// `ToolgateEmbedder` implements it by calling the Toolgate `/v1/embeddings` endpoint.
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use tokio::sync::OnceCell;

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

    /// Returns the name/label of the active embedding model, if known.
    fn embed_model_name(&self) -> Option<String>;

    /// Generate an embedding vector for a single text.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Batch-embed multiple texts in a single request.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

// ── ToolgateEmbedder ─────────────────────────────────────────────────────────

/// Concrete embedder that calls the Toolgate OpenAI-compatible `/v1/embeddings` endpoint.
pub struct ToolgateEmbedder {
    db: sqlx::PgPool,
    http: reqwest::Client,
    embed_url: String,
    embed_model: OnceLock<String>,
    /// 0 = not yet detected
    embed_dim: AtomicU32,
    /// Requested dimensions for the embedding API (0 = use model default)
    embed_dimensions: u32,
    /// Lazy initialization guard: embedding probe runs on first memory operation.
    initialized: OnceCell<()>,
}

impl ToolgateEmbedder {
    /// Create a new embedder pointed at the given Toolgate URL.
    ///
    /// `embed_dim`: initial dimension hint (0 = auto-detect via probe).
    /// `embed_dimensions`: requested output dimensions (0 = use model default).
    pub fn new(db: sqlx::PgPool, toolgate_url: &str, embed_dim: u32, embed_dimensions: u32) -> Self {
        // 60s tolerates cold-start of CPU-only embedding models on Pi/ARM64
        // (observed ≥30s on first request after idle). Steady-state requests
        // are sub-second, so the higher ceiling only kicks in for outliers.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        let embed_url = if toolgate_url.is_empty() {
            String::new()
        } else {
            format!("{}/v1", toolgate_url.trim_end_matches('/'))
        };
        Self {
            db,
            http,
            embed_url,
            embed_model: OnceLock::new(),
            embed_dim: AtomicU32::new(embed_dim),
            embed_dimensions,
            initialized: OnceCell::new(),
        }
    }

    /// Create a disabled embedder (no URL). Used in tests when no embedding is needed.
    #[cfg(test)]
    pub fn new_disabled() -> Self {
        Self {
            db: sqlx::PgPool::connect_lazy("postgres://invalid").unwrap(),
            http: reqwest::Client::new(),
            embed_url: String::new(),
            embed_model: OnceLock::new(),
            embed_dim: AtomicU32::new(0),
            embed_dimensions: 0,
            initialized: OnceCell::new(),
        }
    }

    /// Query toolgate /health to discover the active embedding provider display name.
    /// Note: the display name (e.g. "`OpenAI` Embedding") is for logging only -- it is NOT
    /// passed as the `model` field in embedding requests. Toolgate resolves the actual
    /// model internally from its provider registry.
    async fn fetch_embed_model_from_toolgate(&self) {
        let health_url = format!(
            "{}/health",
            self.embed_url
                .trim_end_matches('/')
                .trim_end_matches("/v1"),
        );
        match self.http.get(&health_url).timeout(std::time::Duration::from_secs(5)).send().await {
            Ok(resp) => {
                if let Ok(body) = resp.json::<serde_json::Value>().await
                    && let Some(name) = body["active_providers"]["embedding"].as_str()
                    {
                        tracing::info!(embed_provider = %name, "discovered embedding provider from toolgate");
                        // Store display name for logging/status only -- do NOT use as model param
                        let _ = self.embed_model.set(format!("({name})"));
                    }
            }
            Err(e) => {
                tracing::debug!(error = %e, "could not query toolgate /health for provider name");
            }
        }
    }

    /// Raw HTTP probe: call `/v1/embeddings` directly to detect dimension.
    /// Does NOT go through `embed()` — used by `do_initialize()` to avoid re-entrant
    /// `OnceCell` deadlock (since `embed()` calls `ensure_initialized()`).
    async fn probe_dimension(&self) -> Result<u32> {
        let url = format!("{}/embeddings", self.embed_url.trim_end_matches('/'));
        let mut body = serde_json::json!({ "input": "dimension probe" });
        if self.embed_dimensions > 0 {
            body["dimensions"] = serde_json::json!(self.embed_dimensions);
        }
        let resp = self.http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("embedding probe request failed")?;
        resp.error_for_status_ref().context("embedding probe API error")?;
        let body: serde_json::Value = resp.json().await.context("failed to parse embedding probe response")?;
        let vec: Vec<f32> = body["data"][0]["embedding"]
            .as_array()
            .context("missing 'data[0].embedding' in probe response")?
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        anyhow::ensure!(!vec.is_empty(), "embedding probe returned empty vector");
        Ok(vec.len() as u32)
    }

    /// Initialize embedding: auto-detect dimension, validate DB, ensure HNSW index.
    /// Graceful: if embedding endpoint is unreachable, logs a warning and continues
    /// (FTS fallback will be used for search).
    async fn do_initialize(&self) -> Result<()> {
        if !self.is_available() {
            tracing::info!("embedding not configured, memory will use FTS only");
            return Ok(());
        }

        // 1. Detect dimension (from config or probe request).
        // Uses probe_dimension() (raw HTTP) instead of embed() to avoid re-entrant
        // OnceCell deadlock — embed() calls ensure_initialized() which calls do_initialize().
        let current_dim = self.embed_dim.load(Ordering::Relaxed);
        let dim = if current_dim > 0 {
            current_dim
        } else {
            match self.probe_dimension().await {
                Ok(d) => {
                    self.embed_dim.store(d, Ordering::Relaxed);
                    d
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "embedding endpoint unreachable at startup, memory degraded to FTS"
                    );
                    return Ok(());
                }
            }
        };

        // 2. Discover embedding model name from toolgate health endpoint
        self.fetch_embed_model_from_toolgate().await;

        // 3. Check if DB has embeddings with a different dimension
        let existing_dim = crate::db::memory_queries::get_existing_embedding_dim(&self.db).await;

        if let Some(old_dim) = existing_dim
            && old_dim as u32 != dim {
                tracing::warn!(
                    old_dim, new_dim = dim,
                    "embedding dimension changed, clearing memory_chunks"
                );
                crate::db::memory_queries::clear_embeddings(&self.db).await?;
                // Drop old index (wrong dimension)
                crate::db::memory_queries::drop_hnsw_index(&self.db).await?;
            }

        // 4. Try to create vector index (non-fatal -- sequential scan works without it)
        if let Err(e) = self.ensure_index(dim).await {
            tracing::info!(dim, error = %e, "vector index not created -- using sequential scan (OK for <100K rows)");
        }

        let model = self.embed_model_name().unwrap_or_default();
        tracing::info!(
            model = %model,
            dim,
            "embedding initialized"
        );
        Ok(())
    }

    /// Lazy initialization: runs embedding probe on first memory operation, not at startup.
    pub async fn ensure_initialized(&self) {
        self.initialized.get_or_init(|| async {
            if let Err(e) = self.do_initialize().await {
                tracing::warn!(error = %e, "embedding init failed -- memory uses FTS only");
            }
        }).await;
    }

    /// Create HNSW index if it doesn't exist.
    async fn ensure_index(&self, dim: u32) -> Result<()> {
        crate::db::memory_queries::ensure_hnsw_index(&self.db, dim).await
    }

    /// Helper: get embed model name string (empty if not yet discovered).
    fn embed_model_str(&self) -> String {
        self.embed_model.get().cloned().unwrap_or_default()
    }
}

#[async_trait]
impl EmbeddingService for ToolgateEmbedder {
    fn is_available(&self) -> bool {
        !self.embed_url.is_empty()
    }

    fn embed_dim(&self) -> u32 {
        self.embed_dim.load(Ordering::Relaxed)
    }

    fn embed_model_name(&self) -> Option<String> {
        self.embed_model.get().cloned()
    }

    /// Call the OpenAI-compatible /v1/embeddings endpoint and return the vector.
    /// On first call, runs `ensure_initialized()` to detect dimension, discover model name,
    /// check for DB dimension mismatch, and create the HNSW index.
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.ensure_initialized().await;
        let url = format!("{}/embeddings", self.embed_url.trim_end_matches('/'));
        let model = self.embed_model_str();
        let mut body = serde_json::json!({ "input": text });
        // Only pass model if it's a real model ID (not a display name from toolgate health)
        if !model.is_empty() && !model.starts_with('(') {
            body["model"] = serde_json::Value::String(model);
        }
        if self.embed_dimensions > 0 {
            body["dimensions"] = serde_json::json!(self.embed_dimensions);
        }
        let resp = self.http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("embedding request failed")?;

        resp.error_for_status_ref().context("embedding API error")?;
        let body: serde_json::Value = resp.json().await.context("failed to parse embedding response")?;

        let vec: Vec<f32> = body["data"][0]["embedding"]
            .as_array()
            .context("missing 'data[0].embedding' in response")?
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();

        anyhow::ensure!(!vec.is_empty(), "embedding returned empty vector");

        // Validate dimension matches expected (if already known)
        let expected = self.embed_dim.load(Ordering::Relaxed);
        if expected > 0 && vec.len() as u32 != expected {
            anyhow::bail!(
                "embedding dimension mismatch: expected {}, got {} -- possible model change",
                expected, vec.len()
            );
        }

        // Lazy init: if dim was unknown (embedding was down at startup), set it now.
        // compare_exchange ensures only one thread creates the HNSW index.
        let detected_dim = vec.len() as u32;
        if self.embed_dim.compare_exchange(0, detected_dim, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            let model = self.embed_model_str();
            tracing::info!(dim = detected_dim, model = %model, "embedding came online, lazy-initializing");
            if let Err(e) = self.ensure_index(detected_dim).await {
                tracing::warn!(error = %e, "failed to create HNSW index during lazy init");
            }
        }

        Ok(vec)
    }

    /// Batch embed: sends multiple texts in one request (`OpenAI` API supports arrays).
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        self.ensure_initialized().await;
        if texts.len() == 1 {
            return Ok(vec![self.embed(texts[0]).await?]);
        }

        let url = format!("{}/embeddings", self.embed_url.trim_end_matches('/'));
        let model = self.embed_model_str();
        let mut body = serde_json::json!({ "input": texts });
        if !model.is_empty() && !model.starts_with('(') {
            body["model"] = serde_json::Value::String(model);
        }
        if self.embed_dimensions > 0 {
            body["dimensions"] = serde_json::json!(self.embed_dimensions);
        }
        let resp = self.http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("batch embedding request failed")?;

        resp.error_for_status_ref().context("batch embedding API error")?;
        let body: serde_json::Value = resp.json().await.context("failed to parse batch embedding response")?;

        let data = body["data"]
            .as_array()
            .context("missing 'data' array in batch embedding response")?;

        let mut results = Vec::with_capacity(texts.len());
        for item in data {
            let vec: Vec<f32> = item["embedding"]
                .as_array()
                .context("missing 'embedding' in batch result")?
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            anyhow::ensure!(!vec.is_empty(), "batch embedding returned empty vector");
            results.push(vec);
        }

        // Validate dimension matches expected (if already known)
        let expected = self.embed_dim.load(Ordering::Relaxed);
        if expected > 0 {
            for (i, v) in results.iter().enumerate() {
                if v.len() as u32 != expected {
                    anyhow::bail!(
                        "batch embedding dimension mismatch at index {}: expected {}, got {}",
                        i, expected, v.len()
                    );
                }
            }
        }

        // Lazy init if needed.
        // compare_exchange ensures only one thread creates the HNSW index.
        if !results.is_empty() {
            let detected_dim = results[0].len() as u32;
            if self.embed_dim.compare_exchange(0, detected_dim, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                let model = self.embed_model_str();
                tracing::info!(dim = detected_dim, model = %model, "embedding came online via batch, lazy-initializing");
                if let Err(e) = self.ensure_index(detected_dim).await {
                    tracing::warn!(error = %e, "failed to create HNSW index during lazy init");
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

// ── Test-only embedder ───────────────────────────────────────────────────────

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

    fn embed_model_name(&self) -> Option<String> {
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
