/// `MemoryService` trait — abstraction over the concrete `MemoryStore` for testability.
///
/// Engine holds `Arc<dyn MemoryService>` instead of `Arc<MemoryStore>` so unit
/// tests can inject a `MockMemoryService` without needing a live `PostgreSQL` + pgvector stack.
///
/// Embedding operations (`embed`, `embed_batch`, `embed_dim`, `embed_model_name`)
/// are on the separate `EmbeddingService` trait (`crate::memory::EmbeddingService`).
use anyhow::Result;
use async_trait::async_trait;

/// Abstraction over the native memory store.
///
/// All async methods mirror the public API of `crate::memory::MemoryStore`.
/// The `search` method uses `String` for the mode (instead of `&'static str`) to
/// allow object-safe trait dispatch via `Arc<dyn MemoryService>`.
///
/// Embedding operations live on [`crate::memory::EmbeddingService`]; this trait
/// only covers storage, retrieval, and FTS.
#[async_trait]
pub trait MemoryService: Send + Sync {
    /// Returns true when embedding is enabled and endpoint is configured.
    fn is_available(&self) -> bool;

    /// Hybrid search (semantic + FTS). Returns results and search mode string.
    /// `agent_id`: filter to this agent's chunks plus shared chunks. Pass `""` to search all.
    async fn search(
        &self,
        query: &str,
        limit: usize,
        exclude_ids: &[String],
        agent_id: &str,
    ) -> Result<(Vec<crate::memory::MemoryResult>, String)>;

    /// Index a new memory chunk. Returns the new chunk UUID.
    /// `scope`: "private" (agent-only) or "shared" (visible to all agents).
    /// `agent_id`: the agent that owns this chunk.
    async fn index(
        &self,
        content: &str,
        source: &str,
        pinned: bool,
        scope: &str,
        agent_id: &str,
    ) -> Result<String>;

    /// Batch-index memory chunks. Returns a vec of new chunk UUIDs.
    /// Tuple: (content, source, pinned, scope).
    /// `agent_id`: the agent that owns these chunks.
    async fn index_batch(&self, items: &[(String, String, bool, String)], agent_id: &str) -> Result<Vec<String>>;

    /// Load pinned memory chunks formatted for context injection.
    /// Returns (formatted text, list of chunk IDs).
    async fn load_pinned(
        &self,
        agent_id: &str,
        budget_tokens: u32,
    ) -> Result<(String, Vec<String>)>;

    /// Fetch memory chunks by id or source. Returns raw chunk records.
    async fn get(
        &self,
        chunk_id: Option<&str>,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::memory::MemoryChunk>>;

    /// Delete a memory chunk by UUID. Returns true if a row was deleted.
    async fn delete(&self, chunk_id: &str) -> Result<bool>;

    /// Return the N most recently created chunks.
    async fn recent(&self, limit: i64) -> Result<Vec<crate::memory::MemoryResult>>;

    /// Wipe all memory for an agent: deletes all memory chunks for the given agent.
    /// Returns the number of memory chunks deleted.
    async fn wipe_agent_memory(&self, agent_id: &str) -> Result<u64>;

    /// Insert a reindex task into the memory worker queue.
    /// Returns the task UUID.
    async fn enqueue_reindex_task(&self, params: serde_json::Value) -> Result<uuid::Uuid>;

    // ── FTS helpers ─────────────────────────────────────────────────────────

    /// Current FTS language setting (e.g. "english").
    fn fts_language(&self) -> String { "english".to_string() }

    /// FTS language validated against `pg_catalog.pg_ts_config`.
    fn validated_fts_language(&self) -> anyhow::Result<String> { Ok("english".to_string()) }

    /// Update the in-memory FTS language (does NOT write to DB).
    fn set_fts_language(&self, _lang: &str) {}

    /// Rebuild the FTS column for all existing memory chunks.
    async fn rebuild_fts(&self) -> anyhow::Result<u64> { Ok(0) }
}

// ── MemoryStore impl ─────────────────────────────────────────────────────────

#[async_trait]
impl MemoryService for crate::memory::MemoryStore {
    fn is_available(&self) -> bool {
        self.embedder().is_available()
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
        exclude_ids: &[String],
        agent_id: &str,
    ) -> Result<(Vec<crate::memory::MemoryResult>, String)> {
        let (results, mode) = crate::memory::MemoryStore::search(self, query, limit, exclude_ids, agent_id).await?;
        Ok((results, mode.to_string()))
    }

    async fn index(
        &self,
        content: &str,
        source: &str,
        pinned: bool,
        scope: &str,
        agent_id: &str,
    ) -> Result<String> {
        crate::memory::MemoryStore::index(self, content, source, pinned, scope, agent_id).await
    }

    async fn index_batch(&self, items: &[(String, String, bool, String)], agent_id: &str) -> Result<Vec<String>> {
        crate::memory::MemoryStore::index_batch(self, items, agent_id).await
    }

    async fn load_pinned(
        &self,
        agent_id: &str,
        budget_tokens: u32,
    ) -> Result<(String, Vec<String>)> {
        crate::memory::MemoryStore::load_pinned(self, agent_id, budget_tokens).await
    }

    async fn get(
        &self,
        chunk_id: Option<&str>,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::memory::MemoryChunk>> {
        crate::memory::MemoryStore::get(self, chunk_id, source, limit).await
    }

    async fn delete(&self, chunk_id: &str) -> Result<bool> {
        crate::memory::MemoryStore::delete(self, chunk_id).await
    }

    async fn recent(&self, limit: i64) -> Result<Vec<crate::memory::MemoryResult>> {
        crate::memory::MemoryStore::recent(self, limit).await
    }

    async fn wipe_agent_memory(&self, agent_id: &str) -> Result<u64> {
        crate::memory::MemoryStore::wipe_agent_memory(self, agent_id).await
    }

    async fn enqueue_reindex_task(&self, params: serde_json::Value) -> Result<uuid::Uuid> {
        crate::memory::MemoryStore::enqueue_reindex_task(self, params).await
    }

    fn fts_language(&self) -> String {
        crate::memory::MemoryStore::fts_language(self)
    }

    fn validated_fts_language(&self) -> anyhow::Result<String> {
        crate::memory::MemoryStore::validated_fts_language(self)
    }

    fn set_fts_language(&self, lang: &str) {
        crate::memory::MemoryStore::set_fts_language(self, lang);
    }

    async fn rebuild_fts(&self) -> anyhow::Result<u64> {
        crate::memory::MemoryStore::rebuild_fts(self).await
    }
}

// ── Mock (test only) ─────────────────────────────────────────────────────────

#[cfg(test)]
pub mod mock {
    use super::*;

    /// Stub MemoryService for unit tests. No database or network required.
    pub struct MockMemoryService {
        pub available: bool,
    }

    impl MockMemoryService {
        pub fn available() -> Self {
            Self { available: true }
        }

        pub fn unavailable() -> Self {
            Self { available: false }
        }
    }

    #[async_trait]
    impl MemoryService for MockMemoryService {
        fn is_available(&self) -> bool {
            self.available
        }

        async fn search(
            &self,
            _query: &str,
            _limit: usize,
            _exclude_ids: &[String],
            _agent_id: &str,
        ) -> Result<(Vec<crate::memory::MemoryResult>, String)> {
            Ok((vec![], "mock".to_string()))
        }

        async fn index(
            &self,
            _content: &str,
            _source: &str,
            _pinned: bool,
            _scope: &str,
            _agent_id: &str,
        ) -> Result<String> {
            Ok("mock-chunk-id".to_string())
        }

        async fn index_batch(
            &self,
            items: &[(String, String, bool, String)],
            _agent_id: &str,
        ) -> Result<Vec<String>> {
            Ok(items.iter().map(|_| "mock-chunk-id".to_string()).collect())
        }

        async fn load_pinned(
            &self,
            _agent_id: &str,
            _budget_tokens: u32,
        ) -> Result<(String, Vec<String>)> {
            Ok((String::new(), vec![]))
        }

        async fn get(
            &self,
            _chunk_id: Option<&str>,
            _source: Option<&str>,
            _limit: usize,
        ) -> Result<Vec<crate::memory::MemoryChunk>> {
            Ok(vec![])
        }

        async fn delete(&self, _chunk_id: &str) -> Result<bool> {
            Ok(false)
        }

        async fn recent(&self, _limit: i64) -> Result<Vec<crate::memory::MemoryResult>> {
            Ok(vec![])
        }

        async fn wipe_agent_memory(&self, _agent_id: &str) -> Result<u64> {
            Ok(0)
        }

        async fn enqueue_reindex_task(&self, _params: serde_json::Value) -> Result<uuid::Uuid> {
            Ok(uuid::Uuid::nil())
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::mock::MockMemoryService;
    use super::MemoryService;
    use std::sync::Arc;

    #[test]
    fn mock_is_available_true() {
        let mock = MockMemoryService::available();
        assert!(mock.is_available());
    }

    #[test]
    fn mock_is_available_false() {
        let mock = MockMemoryService::unavailable();
        assert!(!mock.is_available());
    }

    #[tokio::test]
    async fn mock_search_returns_empty_without_db() {
        let mock = MockMemoryService::available();
        let (results, mode) = mock.search("test query", 5, &[], "agent1").await.unwrap();
        assert!(results.is_empty());
        assert_eq!(mode, "mock");
    }

    #[tokio::test]
    async fn mock_recent_returns_empty_without_db() {
        let mock = MockMemoryService::available();
        let results = mock.recent(10).await.unwrap();
        assert!(results.is_empty());
    }

    /// Verify that Arc<dyn MemoryService> dispatch works (trait is object-safe).
    #[tokio::test]
    async fn trait_object_dispatch_works() {
        let svc: Arc<dyn MemoryService> = Arc::new(MockMemoryService::available());
        assert!(svc.is_available());
        let (results, mode) = svc.search("hello", 5, &[], "agent1").await.unwrap();
        assert!(results.is_empty());
        assert_eq!(mode, "mock");
    }

    // ── Scope tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn index_with_private_scope() {
        let mock = MockMemoryService::available();
        let id = mock.index("private fact", "test", false, "private", "Arty").await.unwrap();
        assert_eq!(id, "mock-chunk-id");
    }

    #[tokio::test]
    async fn index_with_shared_scope() {
        let mock = MockMemoryService::available();
        let id = mock.index("shared fact", "test", false, "shared", "Arty").await.unwrap();
        assert_eq!(id, "mock-chunk-id");
    }

    #[tokio::test]
    async fn index_batch_with_scope() {
        let mock = MockMemoryService::available();
        let items = vec![
            ("fact 1".into(), "src".into(), false, "private".into()),
            ("fact 2".into(), "src".into(), false, "shared".into()),
        ];
        let ids = mock.index_batch(&items, "Arty").await.unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[tokio::test]
    async fn search_with_agent_id_filter() {
        let mock = MockMemoryService::available();
        // Mock returns empty regardless, but verify signature accepts agent_id
        let (results, _) = mock.search("query", 5, &[], "Arty").await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_with_empty_agent_id_for_admin() {
        let mock = MockMemoryService::available();
        // Empty agent_id = admin context, returns all
        let (results, _) = mock.search("query", 5, &[], "").await.unwrap();
        assert!(results.is_empty());
    }

    // ── Memory lifecycle tests ──────────────────────────────

    #[tokio::test]
    async fn index_then_get_by_source() {
        let mock = MockMemoryService::available();
        let id = mock.index("test content", "test-source", false, "private", "Agent1").await.unwrap();
        assert!(!id.is_empty());
        // Mock get returns empty but verifies signature
        let chunks = mock.get(None, Some("test-source"), 10).await.unwrap();
        assert!(chunks.is_empty()); // Mock doesn't persist
    }

    #[tokio::test]
    async fn index_then_delete() {
        let mock = MockMemoryService::available();
        let id = mock.index("to delete", "src", false, "private", "Agent1").await.unwrap();
        let deleted = mock.delete(&id).await.unwrap();
        assert!(!deleted); // Mock always returns false for delete
    }

    #[tokio::test]
    async fn pinned_loading() {
        let mock = MockMemoryService::available();
        let (text, ids) = mock.load_pinned("Agent1", 2000).await.unwrap();
        assert!(text.is_empty()); // Mock returns empty
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn wipe_agent_memory() {
        let mock = MockMemoryService::available();
        let count = mock.wipe_agent_memory("Agent1").await.unwrap();
        assert_eq!(count, 0); // Mock returns 0
    }

    #[tokio::test]
    async fn index_batch_empty() {
        let mock = MockMemoryService::available();
        let items: Vec<(String, String, bool, String)> = vec![];
        let ids = mock.index_batch(&items, "Agent1").await.unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn search_with_exclude_ids() {
        let mock = MockMemoryService::available();
        let exclude = vec!["id1".to_string(), "id2".to_string()];
        let (results, mode) = mock.search("query", 5, &exclude, "Agent1").await.unwrap();
        assert!(results.is_empty());
        assert_eq!(mode, "mock");
    }

}
