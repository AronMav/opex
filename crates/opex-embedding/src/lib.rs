//! HTTP transport-layer for HydeClaw embeddings (Toolgate proxy).
//!
//! Used by both `opex-core` (via `ToolgateEmbedder`) and
//! `opex-memory-worker` (via direct `ToolgateClient` calls).

pub mod client;
pub mod retry;
pub mod trace;

pub use client::{ToolgateClient, ToolgateHealth};
pub use retry::{RetryPolicy, RetryableError};

// Re-exports добавятся в Task 5-6 по мере реализации модулей.
