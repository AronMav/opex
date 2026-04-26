use std::sync::Arc;

use sqlx::PgPool;

use crate::agent::memory_service::MemoryService;
use crate::memory::EmbeddingService;

// ── InfraServices cluster ─────────────────────────────────────────────────────

/// Cluster holding infrastructure-level services:
/// the database pool, memory store, embedding service, container/sandbox managers,
/// the native process manager, and the Phase 62 metrics registry.
#[derive(Clone)]
pub struct InfraServices {
    /// sqlx PostgreSQL connection pool.
    pub db: PgPool,
    /// Pluggable memory backend (real pgvector store or test stub).
    pub memory_store: Arc<dyn MemoryService>,
    /// Embedding service for vector generation (shared with MemoryStore).
    pub embedder: Arc<dyn EmbeddingService>,
    /// Docker-based MCP container lifecycle manager.
    pub container_manager: Option<Arc<crate::containers::ContainerManager>>,
    /// Per-agent code-execution sandbox (Docker).
    pub sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    /// Native process manager (channels, toolgate, …).
    pub process_manager: Option<Arc<crate::process_manager::ProcessManager>>,
    /// Phase 62 RES-02: process-wide metrics registry. Backs
    /// `GET /api/health/dashboard` and the Phase 62 RES-01 coalescer drop
    /// counter. Phase 65 OBS-02 layers OpenTelemetry meter wrappers on top.
    pub metrics: Arc<crate::metrics::MetricsRegistry>,
    /// Secrets manager — provides HMAC key derivation for signed URLs
    /// (workspace-files endpoint, upload verification).
    pub secrets: Arc<crate::secrets::SecretsManager>,
}

impl InfraServices {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: PgPool,
        memory_store: Arc<dyn MemoryService>,
        embedder: Arc<dyn EmbeddingService>,
        container_manager: Option<Arc<crate::containers::ContainerManager>>,
        sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
        process_manager: Option<Arc<crate::process_manager::ProcessManager>>,
        metrics: Arc<crate::metrics::MetricsRegistry>,
        secrets: Arc<crate::secrets::SecretsManager>,
    ) -> Self {
        Self { db, memory_store, embedder, container_manager, sandbox, process_manager, metrics, secrets }
    }

    /// Construct a minimal `InfraServices` for unit tests.
    /// Accepts any `MemoryService` impl (e.g. `NullMemory` or `MockMemoryService`).
    /// Metrics registry is a fresh empty `MetricsRegistry`.
    #[cfg(test)]
    pub fn test_with_memory(memory: impl MemoryService + 'static) -> Self {
        use crate::memory::embedding::FakeEmbedder;
        Self {
            db: PgPool::connect_lazy("postgres://invalid").expect("lazy pool"),
            memory_store: Arc::new(memory),
            embedder: Arc::new(FakeEmbedder { available: false }),
            container_manager: None,
            sandbox: None,
            process_manager: None,
            metrics: Arc::new(crate::metrics::MetricsRegistry::new()),
            secrets: Arc::new(crate::secrets::SecretsManager::new_noop()),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::memory_service::MemoryService;

    // ── NullMemory stub ──────────────────────────────────────────────────────

    struct NullMemory;

    #[async_trait::async_trait]
    impl MemoryService for NullMemory {
        fn is_available(&self) -> bool {
            false
        }

        async fn search(
            &self,
            _query: &str,
            _limit: usize,
            _exclude_ids: &[String],
            _category: Option<&str>,
            _topic: Option<&str>,
            _agent_id: &str,
        ) -> anyhow::Result<(Vec<crate::memory::MemoryResult>, String)> {
            Ok((vec![], "null".to_string()))
        }

        async fn index(
            &self,
            _content: &str,
            _source: &str,
            _pinned: bool,
            _category: Option<&str>,
            _topic: Option<&str>,
            _scope: &str,
            _agent_id: &str,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn index_batch(
            &self,
            _items: &[(String, String, bool, String)],
            _agent_id: &str,
        ) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }

        async fn load_pinned(
            &self,
            _agent_id: &str,
            _budget_tokens: u32,
        ) -> anyhow::Result<(String, Vec<String>)> {
            Ok((String::new(), vec![]))
        }

        async fn get(
            &self,
            _chunk_id: Option<&str>,
            _source: Option<&str>,
            _limit: usize,
        ) -> anyhow::Result<Vec<crate::memory::MemoryChunk>> {
            Ok(vec![])
        }

        async fn delete(&self, _chunk_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn recent(&self, _limit: i64) -> anyhow::Result<Vec<crate::memory::MemoryResult>> {
            Ok(vec![])
        }

        async fn wipe_agent_memory(&self, _agent_id: &str) -> anyhow::Result<u64> {
            Ok(0)
        }

        async fn enqueue_reindex_task(
            &self,
            _params: serde_json::Value,
        ) -> anyhow::Result<uuid::Uuid> {
            Ok(uuid::Uuid::nil())
        }
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn infra_services_null_memory_not_available() {
        let infra = InfraServices::test_with_memory(NullMemory);
        assert!(!infra.memory_store.is_available());
    }

    #[tokio::test]
    async fn infra_services_optional_fields_are_none() {
        let infra = InfraServices::test_with_memory(NullMemory);
        assert!(infra.container_manager.is_none());
        assert!(infra.sandbox.is_none());
        assert!(infra.process_manager.is_none());
    }

    #[tokio::test]
    async fn infra_services_clone_shares_memory_store_arc() {
        let infra = InfraServices::test_with_memory(NullMemory);
        let infra2 = infra.clone();
        assert!(Arc::ptr_eq(&infra.memory_store, &infra2.memory_store));
    }

    #[tokio::test]
    async fn infra_services_mock_memory_available() {
        use crate::agent::memory_service::mock::MockMemoryService;
        let infra = InfraServices::test_with_memory(MockMemoryService::available());
        assert!(infra.memory_store.is_available());
    }
}
