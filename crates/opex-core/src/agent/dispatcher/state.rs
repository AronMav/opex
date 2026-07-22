//! Per-session describe cache for the tool dispatcher.

use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Map of session UUID → per-session tool dispatcher state.
pub type SessionToolStateMap = Arc<DashMap<Uuid, Arc<SessionToolState>>>;

/// Per-session describe cache for the tool dispatcher.
/// Avoids repeated filesystem reads (`load_yaml_tools`) within one session.
/// Also carries per-turn capability provider overrides set by the `profile`
/// tool's `switch` action — consumed by `provider_attempts_for` and
/// `slot_chain_header` to route a capability tool to a specific provider
/// for the remainder of the current turn.
pub struct SessionToolState {
    describe_cache: RwLock<HashMap<String, String>>,
    capability_provider_override: RwLock<Option<(String, String)>>,
}

impl SessionToolState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            describe_cache: RwLock::new(HashMap::new()),
            capability_provider_override: RwLock::new(None),
        })
    }

    /// Returns the cached rendered description for `name`, or `None` on miss.
    pub async fn get_describe(&self, name: &str) -> Option<String> {
        self.describe_cache.read().await.get(name).cloned()
    }

    /// Inserts (or overwrites) the rendered description for `name`.
    pub async fn set_describe(&self, name: String, value: String) {
        self.describe_cache.write().await.insert(name, value);
    }

    /// Set a per-turn capability provider override: `(slot, provider)`.
    /// E.g. `("imagegen", "chroma1-hd")` makes `generate_image` use the
    /// `chroma1-hd` provider for this turn regardless of slot order.
    pub async fn set_capability_provider(&self, slot: String, provider: String) {
        *self.capability_provider_override.write().await = Some((slot, provider));
    }

    /// Returns the active per-turn override `(slot, provider)` if set.
    pub async fn capability_provider(&self) -> Option<(String, String)> {
        self.capability_provider_override.read().await.clone()
    }

    /// Clear the per-turn override (called at turn end by the pipeline).
    pub async fn clear_capability_provider(&self) {
        *self.capability_provider_override.write().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn describe_cache_miss_returns_none() {
        let state = SessionToolState::new();
        assert!(state.get_describe("my_tool").await.is_none());
    }

    #[tokio::test]
    async fn describe_cache_roundtrip() {
        let state = SessionToolState::new();
        state.set_describe("my_tool".to_string(), "schema text".to_string()).await;
        assert_eq!(
            state.get_describe("my_tool").await.as_deref(),
            Some("schema text")
        );
    }

    #[tokio::test]
    async fn describe_cache_different_keys_independent() {
        let state = SessionToolState::new();
        state.set_describe("tool_a".to_string(), "schema_a".to_string()).await;
        assert!(state.get_describe("tool_b").await.is_none());
    }
}
