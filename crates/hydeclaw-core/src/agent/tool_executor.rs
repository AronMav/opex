//! `ToolExecutor` trait, `ToolExecutorCtx` value type, `DefaultToolExecutor` implementation,
//! and `ToolExecutorDeps` private trait.
//!
//! Extracted from `engine_dispatch.rs` and `engine_parallel.rs` to decouple tool execution
//! from the engine god object, enabling mock injection in tests (TOOL-01..TOOL-03).
//! Follows the same `OnceLock` + private deps trait pattern as `ContextBuilder` (Phase 38).
//!
//! Phase 39-02: `DefaultToolExecutor` now holds 13 tool-only fields migrated from `AgentEngine`,
//! reducing the engine struct by 13 fields (TOOL-04).

use anyhow::Result;
use async_trait::async_trait;
use hydeclaw_types::ToolCall;
use std::sync::{Arc, OnceLock, Weak};
use uuid::Uuid;

pub use crate::agent::engine::LoopBreak;
use crate::agent::tool_loop::LoopDetector;

// ── ToolExecutor public trait ─────────────────────────────────────────────────

/// Abstraction over tool execution so unit tests can inject a `MockToolExecutor`
/// without needing a live LLM stack or filesystem.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute a batch of tool calls with loop detection and partitioned parallelism.
    #[allow(clippy::too_many_arguments)]
    async fn execute_batch(
        &self,
        tool_calls: &[ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut LoopDetector,
        detect_loops: bool,
    ) -> Result<Vec<(String, String)>, LoopBreak>;
}

// ── ToolExecutorDeps private trait ───────────────────────────────────────────

/// Private trait listing the `AgentEngine` capabilities consumed by `DefaultToolExecutor`.
/// `AgentEngine` implements this; the impl delegates to its own fields/methods.
/// This avoids a direct Arc<AgentEngine> dependency from `tool_executor.rs` back to engine.rs.
#[async_trait]
pub(crate) trait ToolExecutorDeps: Send + Sync {
    /// Batch execution — delegates to `engine_parallel.rs`.
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_calls_partitioned_raw(
        &self,
        tool_calls: &[ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut LoopDetector,
        detect_loops: bool,
    ) -> Result<Vec<(String, String)>, LoopBreak>;
}

// ── DefaultToolExecutor ───────────────────────────────────────────────────────

/// Concrete implementation of `ToolExecutor` that delegates all engine access
/// through the `ToolExecutorDeps` trait and owns the tool-only state fields
/// migrated from `AgentEngine` (Phase 39-02, TOOL-04).
pub struct DefaultToolExecutor {
    deps: Weak<dyn ToolExecutorDeps>,
    /// Self-reference for recursive subagent calls (Arc<dyn ToolExecutor>).
    /// Initialized via `set_self_ref` after the executor is wrapped in Arc.
    self_ref: OnceLock<Arc<dyn ToolExecutor>>,

    // ── Migrated tool-only fields (Phase 39-02) ───────────────────────────────

    /// Code execution sandbox (Docker). None when sandbox disabled or Docker unavailable.
    pub(crate) sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    /// Background processes started by `process_start` tool (base agents only).
    pub(crate) bg_processes: Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::agent::engine::BgProcess>>>,
    /// Cached YAML tool definitions with TTL (avoids per-batch disk reads in parallel execution).
    pub(crate) yaml_tools_cache: tokio::sync::RwLock<(std::time::Instant, std::sync::Arc<std::collections::HashMap<String, crate::tools::yaml_tools::YamlToolDef>>)>,
    /// Cached skill definitions with TTL (avoids per-message disk reads during context building).
    pub(crate) skills_cache: tokio::sync::RwLock<(std::time::Instant, Vec<crate::skills::SkillDef>)>,
    /// Per-engine web search cache (`query_hash` → (result, expiry)). TTL: 5 minutes.
    pub(crate) search_cache: tokio::sync::RwLock<std::collections::HashMap<u64, (String, std::time::Instant)>>,
    /// In-memory cache for tool embeddings (semantic top-K selection).
    pub(crate) tool_embed_cache: Arc<crate::tools::embedding::ToolEmbeddingCache>,
    /// Tool quality penalty cache for adaptive tool ranking.
    pub(crate) penalty_cache: Arc<crate::db::tool_quality::PenaltyCache>,
    /// IDs of L0 pinned chunks loaded in the current context build (for L2 dedup).
    pub(crate) pinned_chunk_ids: tokio::sync::Mutex<Vec<String>>,
    /// Mutex for atomic MEMORY.md read-modify-write operations.
    pub(crate) memory_md_lock: tokio::sync::Mutex<()>,
    /// Current canvas content for eval/snapshot.
    pub(crate) canvas_state: tokio::sync::RwLock<Option<crate::agent::engine::CanvasContent>>,
    /// SSRF-safe HTTP client for user-supplied URLs (custom DNS resolver blocks private IPs).
    pub(crate) ssrf_http_client: reqwest::Client,
    /// OAuth 2.0 connection manager for provider-based YAML tool auth.
    pub(crate) oauth: Option<Arc<crate::oauth::OAuthManager>>,
    /// Registry of async subagents for status/logs/kill management.
    pub(crate) subagent_registry: crate::agent::subagent_state::SubagentRegistry,

    // ── Shared fields (also removed from AgentEngine — accessed via tex()) ────

    /// Secrets vault for resolving auth keys in YAML tools and provider credentials.
    pub(crate) secrets: Arc<crate::secrets::SecretsManager>,
    /// MCP server registry for listing/calling external tool servers.
    pub(crate) mcp: Option<Arc<crate::mcp::McpRegistry>>,
    /// Standard HTTP client for internal/trusted endpoints (toolgate, browser-renderer, etc.).
    pub(crate) http_client: reqwest::Client,
    /// Event hooks for policy enforcement and logging.
    pub(crate) hooks: Arc<crate::agent::hooks::HookRegistry>,
    /// In-memory waiters for pending tool-call approvals (shared with ApprovalManager).
    /// Phase 66 REF-02: backed by `DashMap` (sharded sync lock) — see
    /// `crate::agent::approval_manager::ApprovalWaitersMap`.
    #[allow(dead_code)]
    pub(crate) approval_waiters: crate::agent::approval_manager::ApprovalWaitersMap,
    /// SSE event sender for current streaming session — set/cleared by SSE loop.
    pub(crate) sse_event_tx: Arc<tokio::sync::Mutex<Option<crate::agent::engine_event_sender::EngineEventSender>>>,
}

/// Construction parameters for `DefaultToolExecutor` — all 13 migrated fields.
pub struct DefaultToolExecutorFields {
    pub sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    pub bg_processes: Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::agent::engine::BgProcess>>>,
    pub yaml_tools_cache: tokio::sync::RwLock<(std::time::Instant, std::sync::Arc<std::collections::HashMap<String, crate::tools::yaml_tools::YamlToolDef>>)>,
    pub skills_cache: tokio::sync::RwLock<(std::time::Instant, Vec<crate::skills::SkillDef>)>,
    pub search_cache: tokio::sync::RwLock<std::collections::HashMap<u64, (String, std::time::Instant)>>,
    pub tool_embed_cache: Arc<crate::tools::embedding::ToolEmbeddingCache>,
    pub penalty_cache: Arc<crate::db::tool_quality::PenaltyCache>,
    pub pinned_chunk_ids: tokio::sync::Mutex<Vec<String>>,
    pub memory_md_lock: tokio::sync::Mutex<()>,
    pub canvas_state: tokio::sync::RwLock<Option<crate::agent::engine::CanvasContent>>,
    pub ssrf_http_client: reqwest::Client,
    pub oauth: Option<Arc<crate::oauth::OAuthManager>>,
    pub subagent_registry: crate::agent::subagent_state::SubagentRegistry,
    // Shared fields (Phase 39-02 wave 2)
    pub secrets: Arc<crate::secrets::SecretsManager>,
    pub mcp: Option<Arc<crate::mcp::McpRegistry>>,
    pub http_client: reqwest::Client,
    pub hooks: Arc<crate::agent::hooks::HookRegistry>,
    pub approval_waiters: crate::agent::approval_manager::ApprovalWaitersMap,
    pub sse_event_tx: Arc<tokio::sync::Mutex<Option<crate::agent::engine_event_sender::EngineEventSender>>>,
}

impl DefaultToolExecutor {
    pub fn new(deps: Weak<dyn ToolExecutorDeps>, fields: DefaultToolExecutorFields) -> Self {
        Self {
            deps,
            self_ref: OnceLock::new(),
            sandbox: fields.sandbox,
            bg_processes: fields.bg_processes,
            yaml_tools_cache: fields.yaml_tools_cache,
            skills_cache: fields.skills_cache,
            search_cache: fields.search_cache,
            tool_embed_cache: fields.tool_embed_cache,
            penalty_cache: fields.penalty_cache,
            pinned_chunk_ids: fields.pinned_chunk_ids,
            memory_md_lock: fields.memory_md_lock,
            canvas_state: fields.canvas_state,
            ssrf_http_client: fields.ssrf_http_client,
            oauth: fields.oauth,
            subagent_registry: fields.subagent_registry,
            secrets: fields.secrets,
            mcp: fields.mcp,
            http_client: fields.http_client,
            hooks: fields.hooks,
            approval_waiters: fields.approval_waiters,
            sse_event_tx: fields.sse_event_tx,
        }
    }

    /// Store self-reference for recursive tool calls (mirrors `AgentEngine::set_self_ref`).
    pub fn set_self_ref(&self, arc: &Arc<dyn ToolExecutor>) {
        let _ = self.self_ref.set(arc.clone());
    }
}

// Compile-time Send safety assertion (PITFALLS.md Pitfall 1)
fn _assert_send() {
    fn _check<T: Send>() {}
    _check::<Box<dyn ToolExecutor>>();
}

#[async_trait]
impl ToolExecutor for DefaultToolExecutor {
    async fn execute_batch(
        &self,
        tool_calls: &[ToolCall],
        context: &serde_json::Value,
        session_id: Uuid,
        channel: &str,
        current_context_chars: usize,
        detector: &mut LoopDetector,
        detect_loops: bool,
    ) -> Result<Vec<(String, String)>, LoopBreak> {
        // Weak upgrade is structurally safe: active requests hold a strong Arc<AgentEngine>
        // from the spawned task in chat.rs, so the engine cannot be dropped mid-request.
        let deps = self.deps.upgrade().expect(
            "BUG: engine dropped while tool executor is active — this should be unreachable"
        );
        deps
            .execute_tool_calls_partitioned_raw(
                tool_calls,
                context,
                session_id,
                channel,
                current_context_chars,
                detector,
                detect_loops,
            )
            .await
    }
}
