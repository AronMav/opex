//! REF-01 Task 5: `impl ContextBuilderDeps for AgentEngine` + context /
//! memory / compaction helpers (`build_context`, `build_memory_context`,
//! `compact_*`, `handle_command`, channel info cache helpers).
//!
//! Extracted from `engine/mod.rs` as part of plan 66-02. `compact_session`
//! is an inherent method on `AgentEngine` and remains publicly accessible
//! via `crate::agent::engine::AgentEngine::compact_session` (inherent methods
//! travel with the type).

use anyhow::Result;
use hydeclaw_types::{IncomingMessage, Message};
use uuid::Uuid;

use super::AgentEngine;
use crate::agent::session_manager::SessionManager;
use crate::agent::tool_loop::LoopDetector;
use crate::agent::workspace;

impl AgentEngine {
    /// Build runtime context for system prompt injection.
    pub(super) fn runtime_context(&self, msg: &IncomingMessage) -> workspace::RuntimeContext {
        workspace::RuntimeContext {
            agent_name: self.cfg().agent.name.clone(),
            owner_id: self.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone()),
            channel: msg.channel.clone(),
            model: self.cfg().provider.current_model(),
            datetime_display: workspace::format_local_datetime(&self.cfg().default_timezone),
            formatting_prompt: msg.formatting_prompt.clone(),
            channels: vec![], // populated async in build_context
        }
    }

    /// Get channel info for this agent (cached, refreshed on `channels_changed`).
    pub(super) async fn get_channel_info(&self) -> Vec<workspace::ChannelInfo> {
        // Check cache first
        {
            let cache = self.state().channel_info_cache.read().await;
            if let Some(ref cached) = *cache {
                return cached.clone();
            }
        }
        // Cache miss — load from DB
        let info = self.load_channel_info_from_db().await;
        {
            let mut cache = self.state().channel_info_cache.write().await;
            *cache = Some(info.clone());
        }
        info
    }

    /// Invalidate channel info cache (called on channel CRUD).
    pub async fn invalidate_channel_cache(&self) {
        let mut cache = self.state().channel_info_cache.write().await;
        *cache = None;
    }

    async fn load_channel_info_from_db(&self) -> Vec<workspace::ChannelInfo> {
        let has_connected_channel = self.state().channel_router.is_some();
        let rows = sqlx::query_as::<_, (sqlx::types::Uuid, String, String, String)>(
            "SELECT id, channel_type, display_name, status FROM agent_channels WHERE agent_name = $1",
        )
        .bind(&self.cfg().agent.name)
        .fetch_all(&self.cfg().db)
        .await
        .unwrap_or_default();

        rows.into_iter().map(|(id, ch_type, name, status)| {
            workspace::ChannelInfo {
                channel_id: id.to_string(),
                channel_type: ch_type,
                display_name: name,
                online: status == "running" && has_connected_channel,
            }
        }).collect()
    }

    // ── Memory helpers (from engine_memory.rs) ──────────────────────────────

    /// Build L0 memory context: load pinned chunks for this agent.
    pub(super) async fn build_memory_context(&self, budget_tokens: u32) -> crate::agent::pipeline::memory::MemoryContext {
        crate::agent::pipeline::memory::build_memory_context(
            self.cfg().memory_store.as_ref(),
            &self.cfg().agent.name,
            budget_tokens,
        ).await
    }

    /// Index extracted facts into memory (called after session compaction via /compact).
    pub(super) async fn index_facts_to_memory(&self, facts: &[String]) {
        crate::agent::pipeline::memory::index_facts_to_memory(
            self.cfg().memory_store.as_ref(),
            &self.cfg().agent.name,
            facts,
        ).await
    }

    // ── Context helpers (from engine_context.rs) ─────────────────────────────

    /// Build common context: session, messages, system prompt.
    pub(crate) async fn build_context(
        &self,
        msg: &IncomingMessage,
        include_tools: bool,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
    ) -> Result<crate::agent::context_builder::ContextSnapshot> {
        let cb = self.context_builder.get()
            .expect("context_builder not initialized — call set_context_builder after engine Arc creation");
        crate::agent::pipeline::context::build_context(cb.as_ref(), msg, include_tools, resume_session_id, force_new_session).await
    }

    /// Replace old tool results with "[compacted]" when context exceeds 70% of model window.
    pub(super) fn compact_tool_results(&self, messages: &mut [Message], context_chars: &mut usize) {
        crate::agent::pipeline::context::compact_tool_results(
            &self.cfg().agent.model,
            self.cfg().agent.compaction.as_ref(),
            messages,
            context_chars,
        )
    }

    /// Get compaction parameters from agent config.
    #[allow(dead_code)]
    pub(super) fn compaction_params(&self) -> (usize, usize) {
        crate::agent::pipeline::context::compaction_params(&self.cfg().agent.model, self.cfg().agent.compaction.as_ref())
    }

    /// Run compaction on messages if token budget exceeded, indexing extracted facts to memory.
    pub(crate) async fn compact_messages(&self, messages: &mut Vec<Message>, detector: Option<&LoopDetector>) {
        let engine = self;
        let cfg = engine.cfg();
        crate::agent::pipeline::context::compact_messages(
            &cfg.agent.model,
            cfg.agent.compaction.as_ref(),
            &cfg.agent.language,
            cfg.provider.as_ref(),
            cfg.compaction_provider.as_deref(),
            &cfg.db,
            engine.state().ui_event_tx.as_ref(),
            &cfg.agent.name,
            &cfg.audit_queue,
            messages,
            detector,
            |facts| async move { engine.index_facts_to_memory(&facts).await },
        )
        .await
    }

    /// Compact a specific session's messages via API.
    pub async fn compact_session(&self, session_id: uuid::Uuid) -> Result<(usize, usize)> {
        let engine = self;
        let cfg = engine.cfg();
        crate::agent::pipeline::context::compact_session(
            &cfg.db,
            cfg.provider.as_ref(),
            cfg.compaction_provider.as_deref(),
            &cfg.agent.language,
            &cfg.agent.name,
            session_id,
            &cfg.audit_queue,
            |facts| async move { engine.index_facts_to_memory(&facts).await },
        )
        .await
    }

    // ── Command handler (from engine_commands.rs) ────────────────────────────

    /// Handle /slash commands. Returns Some(result) if a command matched, None otherwise.
    pub(crate) async fn handle_command(&self, text: &str, msg: &IncomingMessage) -> Option<Result<String>> {
        let dm_scope = self.cfg().agent.session.as_ref()
            .map(|s| s.dm_scope.as_str())
            .unwrap_or("per-channel-peer");

        let ctx = crate::agent::pipeline::commands::CommandContext {
            agent_name: &self.cfg().agent.name,
            agent_language: &self.cfg().agent.language,
            agent_model: &self.cfg().agent.model,
            dm_scope,
            max_history_messages: self.cfg().agent.max_history_messages,
            compaction_config: self.cfg().agent.compaction.as_ref(),
            db: &self.cfg().db,
            provider: self.cfg().provider.as_ref(),
            compaction_provider: self.cfg().compaction_provider.as_deref(),
            thinking_level: &self.state().thinking_level,
            memory_store: self.cfg().memory_store.as_ref(),
        };

        crate::agent::pipeline::commands::handle_command(
            &ctx,
            text,
            msg,
            || async { self.invalidate_yaml_tools_cache().await },
        ).await
    }
}

// ── ContextBuilderDeps impl ───────────────────────────────────────────────────

#[async_trait::async_trait]
impl crate::agent::context_builder::ContextBuilderDeps for AgentEngine {
    async fn session_resume(&self, sid: Uuid) -> Result<Uuid> {
        SessionManager::new(self.cfg().db.clone()).resume(sid).await
    }

    async fn session_create_new(&self, user_id: &str, channel: &str) -> Result<Uuid> {
        SessionManager::new(self.cfg().db.clone())
            .create_new(&self.cfg().agent.name, user_id, channel)
            .await
    }

    async fn session_get_or_create(
        &self,
        user_id: &str,
        channel: &str,
        dm_scope: &str,
    ) -> Result<Uuid> {
        SessionManager::new(self.cfg().db.clone())
            .get_or_create(&self.cfg().agent.name, user_id, channel, dm_scope)
            .await
    }

    async fn session_load_messages(
        &self,
        session_id: Uuid,
        limit: i64,
    ) -> Result<Vec<crate::db::sessions::MessageRow>> {
        SessionManager::new(self.cfg().db.clone())
            .load_messages(session_id, Some(limit))
            .await
    }

    async fn session_load_branch_messages(
        &self,
        session_id: Uuid,
        leaf_message_id: Uuid,
    ) -> Result<Vec<crate::db::sessions::MessageRow>> {
        crate::db::sessions::load_branch_messages(&self.cfg().db, session_id, leaf_message_id).await
    }

    async fn session_insert_missing_tool_results(
        &self,
        session_id: Uuid,
        call_ids: &[String],
    ) -> Result<()> {
        SessionManager::new(self.cfg().db.clone())
            .insert_missing_tool_results(session_id, call_ids)
            .await
    }

    async fn session_get_participants(&self, session_id: Uuid) -> Result<Vec<String>> {
        crate::db::sessions::get_participants(&self.cfg().db, session_id).await
    }

    fn agent_name(&self) -> &str {
        &self.cfg().agent.name
    }

    fn agent_base(&self) -> bool {
        self.cfg().agent.base
    }

    fn agent_language(&self) -> &str {
        &self.cfg().agent.language
    }

    fn agent_max_history_messages(&self) -> i64 {
        self.cfg().agent.max_history_messages.unwrap_or(50) as i64
    }

    fn agent_dm_scope(&self) -> &str {
        self.cfg().agent.session.as_ref()
            .map_or("per-channel-peer", |s| s.dm_scope.as_str())
    }

    fn agent_prune_tool_output_after_turns(&self) -> Option<usize> {
        self.cfg().agent.session.as_ref()
            .and_then(|s| s.prune_tool_output_after_turns)
    }

    fn agent_max_tools_in_context(&self) -> Option<usize> {
        self.cfg().agent.max_tools_in_context
    }

    async fn load_workspace_prompt(&self) -> Result<String> {
        workspace::load_workspace_prompt(&self.cfg().workspace_dir, &self.cfg().agent.name).await
    }

    fn workspace_dir(&self) -> &str {
        &self.cfg().workspace_dir
    }

    async fn mcp_tool_definitions(&self) -> Vec<hydeclaw_types::ToolDefinition> {
        if let Some(mcp) = self.mcp() {
            mcp.all_tool_definitions().await
        } else {
            vec![]
        }
    }

    async fn has_tool(&self, name: &str) -> bool {
        AgentEngine::has_tool(self, name).await
    }

    fn memory_is_available(&self) -> bool {
        self.cfg().memory_store.is_available()
    }

    fn channel_router_present(&self) -> bool {
        self.state().channel_router.is_some()
    }

    fn scheduler_present(&self) -> bool {
        self.cfg().scheduler.is_some()
    }

    fn sandbox_absent(&self) -> bool {
        self.tex().sandbox.is_none()
    }

    fn runtime_context(&self, msg: &IncomingMessage) -> workspace::RuntimeContext {
        AgentEngine::runtime_context(self, msg)
    }

    async fn get_channel_info(&self) -> Vec<workspace::ChannelInfo> {
        AgentEngine::get_channel_info(self).await
    }

    fn pinned_budget_tokens(&self) -> u32 {
        self.cfg().app_config.memory.pinned_budget_tokens
    }

    async fn build_memory_context(&self, budget_tokens: u32) -> (String, Vec<String>) {
        let ctx = AgentEngine::build_memory_context(self, budget_tokens).await;
        (ctx.pinned_text, ctx.pinned_ids)
    }

    async fn store_pinned_chunk_ids(&self, ids: Vec<String>) {
        *self.tex().pinned_chunk_ids.lock().await = ids;
    }

    fn internal_tool_definitions(&self) -> Vec<hydeclaw_types::ToolDefinition> {
        AgentEngine::internal_tool_definitions(self)
    }

    async fn load_yaml_tools_cached(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef> {
        let cache = self.tex().yaml_tools_cache.read().await;
        if cache.0.elapsed() < std::time::Duration::from_secs(30) && !cache.1.is_empty() {
            return cache.1.values().cloned().collect();
        }
        drop(cache);
        let loaded = crate::tools::yaml_tools::load_yaml_tools(&self.cfg().workspace_dir, false).await;
        let map: std::collections::HashMap<String, crate::tools::yaml_tools::YamlToolDef> =
            loaded.iter().cloned().map(|t| (t.name.clone(), t)).collect();
        *self.tex().yaml_tools_cache.write().await = (std::time::Instant::now(), std::sync::Arc::new(map));
        loaded
    }

    async fn tool_penalties(&self) -> std::collections::HashMap<String, f32> {
        self.tex().penalty_cache.get_penalties().await
    }

    fn filter_tools_by_policy(&self, tools: Vec<hydeclaw_types::ToolDefinition>) -> Vec<hydeclaw_types::ToolDefinition> {
        AgentEngine::filter_tools_by_policy(self, tools)
    }

    async fn select_top_k_tools_semantic(
        &self,
        tools: Vec<hydeclaw_types::ToolDefinition>,
        query: &str,
        k: usize,
    ) -> Vec<hydeclaw_types::ToolDefinition> {
        crate::agent::pipeline::subagent::select_top_k_tools_semantic(
            self.cfg().embedder.as_ref(),
            self.tool_embed_cache().as_ref(),
            self.cfg().memory_store.is_available(),
            tools,
            query,
            k,
        ).await
    }
}
