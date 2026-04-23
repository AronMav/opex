//! REF-01 Task 4: YAML-tool env/oauth resolver + per-engine caches.
//!
//! Owns `SecretsEnvResolver` (impl `EnvResolver`), `make_resolver`,
//! `make_oauth_context`, `invalidate_yaml_tools_cache`, `check_search_cache`,
//! `store_search_cache`, `format_tool_error`, `truncate_preview`, and the
//! `search_cache_key` helper + `CACHEABLE_SEARCH_TOOLS` list.
//!
//! Extracted from `engine/mod.rs` as part of plan 66-02. External callers
//! (`pipeline::context`, `pipeline::channel_actions`) reach
//! `SecretsEnvResolver` via the `pub(crate) use` re-export in
//! `engine/mod.rs`, so `crate::agent::engine::SecretsEnvResolver` keeps
//! resolving unchanged.

use std::sync::Arc;

use super::AgentEngine;

/// Resolves env var names through `SecretsManager` (scoped to agent).
pub(crate) struct SecretsEnvResolver {
    pub(crate) secrets: Arc<crate::secrets::SecretsManager>,
    pub(crate) agent_name: String,
}

#[async_trait::async_trait]
impl crate::tools::yaml_tools::EnvResolver for SecretsEnvResolver {
    async fn resolve(&self, key: &str) -> Option<String> {
        self.secrets.get_scoped(key, &self.agent_name).await
    }
}

/// YAML tools whose results are cached per-engine to avoid duplicate HTTP calls.
pub(crate) const CACHEABLE_SEARCH_TOOLS: &[&str] = &["searxng_search", "brave_search"];

/// Hash a search query for cache lookup (case-insensitive).
pub(crate) fn search_cache_key(query: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    query.to_lowercase().hash(&mut h);
    h.finish()
}

impl AgentEngine {
    /// Invalidate the cached YAML tool definitions so the next request reloads from disk.
    pub(crate) async fn invalidate_yaml_tools_cache(&self) {
        *self.tex().yaml_tools_cache.write().await = (
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(60)).unwrap(),
            std::sync::Arc::new(std::collections::HashMap::new()),
        );
    }

    pub(crate) async fn check_search_cache(&self, query: &str) -> Option<String> {
        let cache = self.tex().search_cache.read().await;
        if let Some((result, expiry)) = cache.get(&search_cache_key(query))
            && *expiry > std::time::Instant::now()
        {
            tracing::debug!(query, "search cache hit");
            return Some(result.clone());
        }
        None
    }

    pub(crate) async fn store_search_cache(&self, query: &str, result: &str) {
        let mut cache = self.tex().search_cache.write().await;
        cache.insert(search_cache_key(query), (
            result.to_string(),
            std::time::Instant::now() + std::time::Duration::from_secs(300),
        ));
        if cache.len() > 100 {
            let now = std::time::Instant::now();
            cache.retain(|_, (_, exp)| *exp > now);
        }
    }

    /// Build a SecretsEnvResolver for YAML tool env resolution.
    pub(super) fn make_resolver(&self) -> SecretsEnvResolver {
        crate::agent::pipeline::context::make_resolver(self.secrets(), &self.cfg().agent.name)
    }

    /// Build OAuthContext for provider-based YAML tool auth (e.g. `oauth_provider: github`).
    pub(super) fn make_oauth_context(&self) -> Option<crate::tools::yaml_tools::OAuthContext> {
        crate::agent::pipeline::context::make_oauth_context(self.oauth().as_ref(), &self.cfg().agent.name)
    }

    /// Format a tool error as structured JSON for better LLM parsing.
    pub(super) fn format_tool_error(tool_name: &str, error: &str) -> String {
        crate::agent::pipeline::context::format_tool_error(tool_name, error)
    }

    /// Truncate a string to `max` chars with "..." suffix, preserving char boundaries.
    #[allow(dead_code)]
    pub(super) fn truncate_preview(s: &str, max: usize) -> String {
        crate::agent::pipeline::context::truncate_preview(s, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_cache_key_case_insensitive() {
        assert_eq!(search_cache_key("Bitcoin Price"), search_cache_key("bitcoin price"));
        assert_eq!(search_cache_key("HELLO"), search_cache_key("hello"));
    }

    #[test]
    fn search_cache_key_different_queries_different_keys() {
        assert_ne!(search_cache_key("bitcoin"), search_cache_key("ethereum"));
    }

    #[test]
    fn search_cache_key_deterministic() {
        let k1 = search_cache_key("test query");
        let k2 = search_cache_key("test query");
        assert_eq!(k1, k2);
    }

    #[test]
    fn cacheable_search_tools_contains_expected() {
        assert!(CACHEABLE_SEARCH_TOOLS.contains(&"searxng_search"));
        assert!(CACHEABLE_SEARCH_TOOLS.contains(&"brave_search"));
        assert!(!CACHEABLE_SEARCH_TOOLS.contains(&"memory_search"));
    }
}

