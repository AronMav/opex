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
pub struct SessionToolState {
    describe_cache: RwLock<HashMap<String, String>>,
}

impl SessionToolState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { describe_cache: RwLock::new(HashMap::new()) })
    }

    /// Returns the cached rendered description for `name`, or `None` on miss.
    pub async fn get_describe(&self, name: &str) -> Option<String> {
        self.describe_cache.read().await.get(name).cloned()
    }

    /// Inserts (or overwrites) the rendered description for `name`.
    pub async fn set_describe(&self, name: String, value: String) {
        self.describe_cache.write().await.insert(name, value);
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
