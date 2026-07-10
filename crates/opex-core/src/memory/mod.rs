//! Native pgvector memory store.
//!
//! pgvector queries run directly against the local `PostgreSQL` pool.
//! Embedding generation is delegated to Toolgate (`POST /v1/embeddings`), which
//! proxies to the configured embedding backend (Ollama, `OpenAI`, or any other
//! OpenAI-compatible provider). Core never calls Ollama or `OpenAI` directly.

pub mod admin;
pub mod embedding;
pub mod soul;
pub mod store;
pub mod watcher;

pub use embedding::{fmt_vec, EmbeddingService, ToolgateEmbedder};
// CountingEmbedder используется напрямую через crate::memory::embedding::CountingEmbedder
// в тестах tools/embedding.rs и memory/store.rs — re-export не нужен.
pub use store::MemoryStore;
pub use watcher::spawn_workspace_watcher;

// ── Config ────────────────────────────────────────────────────────────────────

use crate::config::default_true;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default, schemars::JsonSchema)]
pub struct MemoryConfig {
    /// Whether embedding is enabled. Defaults to true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Vector dimension (optional, auto-detected at startup)
    pub embed_dim: Option<u32>,
    /// Requested embedding dimensions (sent as `dimensions` param to API).
    /// Some models (e.g. Qwen3-Embedding) support flexible output dims.
    /// If set, the API will return vectors of this size instead of the model default.
    pub embed_dimensions: Option<u32>,
    /// `PostgreSQL` FTS dictionary name (e.g. "russian", "english", "simple").
    /// Auto-detected from first agent's language if not set.
    pub fts_language: Option<String>,
    /// Maximum tokens for pinned chunks in L0 context. Default: 2000.
    /// Approximation: `content.len()` / 4.
    #[serde(default = "default_pinned_budget")]
    pub pinned_budget_tokens: u32,
    /// Age in days after which non-pinned chunks become eligible for compression. Default: 30.
    #[serde(default = "default_compression_age_days")]
    pub compression_age_days: u32,
}

fn default_pinned_budget() -> u32 {
    2000
}

fn default_compression_age_days() -> u32 {
    30
}

// ── Types ─────────────────────────────────────────────────────────────────────

// Types moved to opex-db so integration tests can access them.
pub use opex_db::memory_queries::{MemoryChunk, MemoryResult};
pub use opex_db::memory_queries::SoulCandidate;
