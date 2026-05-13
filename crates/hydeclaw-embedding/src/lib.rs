//! HTTP transport-layer for HydeClaw embeddings (Toolgate proxy).
//!
//! Used by both `hydeclaw-core` (via `ToolgateEmbedder`) and
//! `hydeclaw-memory-worker` (via direct `ToolgateClient` calls).

pub mod client;
pub mod retry;
pub mod trace;

// Re-exports добавятся в Task 2-6 по мере реализации модулей.
