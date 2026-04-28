#[allow(dead_code)] // Scaffolding for AgentEngine decomposition — wired in by later tasks.
pub mod agent_state;
pub mod memory_service;
pub mod context_builder;
pub mod tool_executor;
pub mod session_manager;
pub(crate) mod approval_manager;
pub mod channel_actions;
pub mod channel_kind;
pub mod cli_backend;
pub mod hooks;
// Phase 62 RES-01: `StreamEvent` extracted as a leaf module so the lib
// facade can expose it to integration tests without cascading engine.rs.
pub mod stream_event;
pub mod engine;
// Phase 62 RES-01: sole engine-side send surface (text-delta droppable,
// everything else never dropped per CONTEXT.md locked decisions).
pub mod engine_event_sender;
pub(crate) mod error_classify;
pub(crate) mod localization;
pub mod handle;
pub mod history;
pub mod model_discovery;
pub mod providers;
pub(crate) mod providers_http;
pub(crate) mod openapi;
pub(crate) mod pii;
pub(crate) mod json_repair;
pub(crate) mod thinking;
pub mod subagent_state;
pub mod session_agent_pool;
pub mod tool_loop;
pub mod request_context;
pub(crate) mod url_tools;
pub mod mention_parser;
pub mod workspace;
pub mod knowledge_extractor;
pub mod agent_config;
pub mod pipeline;
// Phase 64 SEC-02: workspace path canonicalization guard. Leaf module with
// zero crate::* deps (only std + dunce) so the lib facade can re-export it
// without cascading the agent subtree.
pub mod path_guard;

/// Delete upload files older than `max_age` from workspace/uploads/.
pub async fn cleanup_stale_uploads(workspace_dir: &str, max_age: std::time::Duration) -> usize {
    let uploads_dir = std::path::PathBuf::from(workspace_dir).join("uploads");
    if !uploads_dir.exists() {
        return 0;
    }
    let mut deleted = 0;
    let cutoff = std::time::SystemTime::now() - max_age;
    let mut entries = match tokio::fs::read_dir(&uploads_dir).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, path = %uploads_dir.display(), "failed to read uploads directory for cleanup");
            return 0;
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_file() { continue; }
        let Ok(meta) = tokio::fs::metadata(&path).await else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if modified >= cutoff { continue; }
        if tokio::fs::remove_file(&path).await.is_ok() {
            deleted += 1;
        }
    }
    if deleted > 0 {
        tracing::info!(deleted, "cleaned up stale uploads");
    }
    deleted
}
