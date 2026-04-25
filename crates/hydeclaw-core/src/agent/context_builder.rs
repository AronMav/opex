//! `ContextBuilder` trait, `ContextSnapshot` return type, and `DefaultContextBuilder` implementation.
//!
//! Extracted from `engine_context.rs` (`build_context` body) to decouple context building
//! from the engine god object (CTX-01) and replace the fragile unnamed tuple with a
//! self-documenting struct (CTX-02). Enables `MockContextBuilder` injection in tests (CTX-03).

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use hydeclaw_types::{IncomingMessage, Message, ToolDefinition};
use std::sync::Weak;
use uuid::Uuid;

// ── Public types ──────────────────────────────────────────────────────────────

/// Named return type for context building — replaces the anonymous
/// `(Uuid, Vec<Message>, Vec<ToolDefinition>)` tuple.
#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    pub session_id: Uuid,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}

/// Abstraction over context building so unit tests can inject a `MockContextBuilder`
/// without needing a live LLM stack.
#[async_trait]
pub trait ContextBuilder: Send + Sync {
    async fn build(
        &self,
        msg: &IncomingMessage,
        include_tools: bool,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
    ) -> Result<ContextSnapshot>;
}

// ── Private engine-deps trait ─────────────────────────────────────────────────

/// Private trait listing the `AgentEngine` capabilities consumed by `DefaultContextBuilder`.
/// `AgentEngine` implements this; the impl delegates to its own fields/methods.
/// This avoids a direct Arc<AgentEngine> dependency from `context_builder.rs` back to engine.rs.
#[async_trait]
pub(crate) trait ContextBuilderDeps: Send + Sync {
    // Session management
    async fn session_resume(&self, sid: Uuid) -> Result<Uuid>;
    async fn session_create_new(&self, user_id: &str, channel: &str) -> Result<Uuid>;
    async fn session_get_or_create(
        &self,
        user_id: &str,
        channel: &str,
        dm_scope: &str,
    ) -> Result<Uuid>;
    async fn session_load_messages(
        &self,
        session_id: Uuid,
        limit: i64,
    ) -> Result<Vec<crate::db::sessions::MessageRow>>;
    async fn session_load_branch_messages(
        &self,
        session_id: Uuid,
        leaf_message_id: Uuid,
    ) -> Result<Vec<crate::db::sessions::MessageRow>>;
    async fn session_insert_missing_tool_results(
        &self,
        session_id: Uuid,
        call_ids: &[String],
    ) -> Result<()>;
    async fn session_get_participants(&self, session_id: Uuid) -> Result<Vec<String>>;

    // Agent settings
    fn agent_name(&self) -> &str;
    fn agent_base(&self) -> bool;
    fn agent_language(&self) -> &str;
    fn agent_max_history_messages(&self) -> i64;
    fn agent_dm_scope(&self) -> &str;
    fn agent_prune_tool_output_after_turns(&self) -> Option<usize>;
    fn agent_max_tools_in_context(&self) -> Option<usize>;

    // Workspace
    async fn load_workspace_prompt(&self) -> Result<String>;

    // MCP
    async fn mcp_tool_definitions(&self) -> Vec<ToolDefinition>;

    // Capabilities
    async fn has_tool(&self, name: &str) -> bool;
    fn memory_is_available(&self) -> bool;
    fn channel_router_present(&self) -> bool;
    fn scheduler_present(&self) -> bool;
    fn sandbox_absent(&self) -> bool; // agent.base && sandbox.is_none()

    // Runtime context
    fn runtime_context(&self, msg: &IncomingMessage) -> crate::agent::workspace::RuntimeContext;
    async fn get_channel_info(&self) -> Vec<crate::agent::workspace::ChannelInfo>;

    // Memory
    fn pinned_budget_tokens(&self) -> u32;
    /// Returns (`pinned_text`, `pinned_ids`)
    async fn build_memory_context(&self, budget_tokens: u32) -> (String, Vec<String>);
    async fn store_pinned_chunk_ids(&self, ids: Vec<String>);

    // Tools
    fn internal_tool_definitions(&self) -> Vec<ToolDefinition>;
    async fn load_yaml_tools_cached(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef>;
    async fn load_skills_cached(&self) -> Vec<crate::skills::SkillDef>;
    async fn tool_penalties(&self) -> std::collections::HashMap<String, f32>;
    fn filter_tools_by_policy(&self, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition>;
    /// Return the set of tool names this agent may actually call,
    /// after applying `filter_tools_by_policy` to the union of
    /// internal/system tools, cached YAML tools, and cached MCP tools.
    /// Used by skill-filtering call sites and trigger-hint logic.
    async fn available_tool_names(&self) -> std::collections::HashSet<String>;
    async fn select_top_k_tools_semantic(
        &self,
        tools: Vec<ToolDefinition>,
        query: &str,
        k: usize,
    ) -> Vec<ToolDefinition>;
}

// ── DefaultContextBuilder ─────────────────────────────────────────────────────

/// Concrete implementation of `ContextBuilder` that delegates all engine access
/// through the `ContextBuilderDeps` trait.
pub struct DefaultContextBuilder {
    deps: Weak<dyn ContextBuilderDeps>,
}

impl DefaultContextBuilder {
    pub fn new(deps: Weak<dyn ContextBuilderDeps>) -> Self {
        Self { deps }
    }
}

// Send/Sync assertion (PITFALLS.md Pitfall 1)
fn _assert_send() {
    fn _check<T: Send + Sync>() {}
    _check::<DefaultContextBuilder>();
}

#[async_trait]
impl ContextBuilder for DefaultContextBuilder {
    async fn build(
        &self,
        msg: &IncomingMessage,
        include_tools: bool,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
    ) -> Result<ContextSnapshot> {
        let deps = self.deps.upgrade().context("engine dropped during context build")?;

        // 1. Get or create session (or resume existing)
        let session_id = if let Some(sid) = resume_session_id {
            deps.session_resume(sid).await?
        } else if force_new_session {
            deps.session_create_new(&msg.user_id, &msg.channel).await?
        } else {
            let dm_scope = deps.agent_dm_scope().to_string();
            deps.session_get_or_create(&msg.user_id, &msg.channel, &dm_scope).await?
        };

        // 2. Load conversation history (branch-aware when leaf_message_id is set)
        let history = if let Some(leaf_id) = msg.leaf_message_id {
            deps.session_load_branch_messages(session_id, leaf_id).await?
        } else {
            let limit = deps.agent_max_history_messages();
            deps.session_load_messages(session_id, limit).await?
        };

        // 3. Build system prompt with MCP tool schemas
        let ws_prompt = deps.load_workspace_prompt().await?;

        // MCP tool schemas in system prompt: name + description only.
        let mcp_defs = deps.mcp_tool_definitions().await;
        let mcp_schemas: Vec<String> = mcp_defs
            .iter()
            .map(|t| format!("- **{}**: {}", t.name, t.description))
            .collect();

        // 4. Capabilities + system prompt
        let user_text = msg.text.clone().unwrap_or_default();

        let capabilities = crate::agent::workspace::CapabilityFlags {
            has_search: deps.has_tool("search_web").await || deps.has_tool("search_web_fresh").await,
            has_memory: deps.memory_is_available(),
            has_message_actions: deps.channel_router_present(),
            has_cron: deps.scheduler_present(),
            has_yaml_tools: true,
            has_browser: std::env::var("BROWSER_RENDERER_URL")
                .unwrap_or_else(|_| "http://localhost:9020".to_string())
                != "disabled",
            has_host_exec: deps.agent_base() && deps.sandbox_absent(),
            is_base: deps.agent_base(),
        };

        let mut runtime = deps.runtime_context(msg);
        runtime.channels = deps.get_channel_info().await;
        let mut system_prompt = crate::agent::workspace::build_system_prompt(
            &ws_prompt,
            &mcp_schemas,
            &capabilities,
            deps.agent_language(),
            &runtime,
        );

        let msg_lower = user_text.to_lowercase();

        // 4c. Skill capture prompt
        {
            let is_capture_request =
                (msg_lower.contains("save") && msg_lower.contains("skill"))
                || (msg_lower.contains("сохрани")
                    && (msg_lower.contains("навык") || msg_lower.contains("скилл")));
            if is_capture_request {
                system_prompt.push_str(
                    "\n\n## Skill Capture\n\
                     The user wants to save the approach from the previous task as a reusable skill.\n\
                     Use workspace_write to create a file in workspace/skills/ with YAML frontmatter \
                     (name, description, triggers, tools_required) and markdown body.\n\
                     Extract the strategy, not specific data.\n",
                );
            }
        }

        // 4d. Skill trigger matching — inject a hint when the user message matches skill triggers
        {
            let skills = deps.load_skills_cached().await;
            let available = deps.available_tool_names().await;
            let visible = crate::skills::filter_skills_by_available_tools(skills, &available);
            // visible are sorted by priority desc; find the first match
            if let Some(skill) = visible.iter().find(|s| {
                !s.meta.triggers.is_empty()
                    && s.meta.triggers.iter().any(|t| msg_lower.contains(t.to_lowercase().as_str()))
            }) {
                system_prompt.push_str(&format!(
                    "\n\n## Relevant Skill Detected\n\
                     The user's request matches the **{}** skill: {}.\n\
                     Call `skill_use(action=\"load\", name=\"{}\")` to load the full instructions \
                     before responding. Do not answer from memory — load the skill first.\n",
                    skill.meta.name,
                    skill.meta.description,
                    skill.meta.name
                ));
            }
        }

        // 4e. Multi-agent session context
        if let Ok(participants) = deps.session_get_participants(session_id).await
            && participants.len() > 1
        {
            system_prompt.push_str("\n\n## Multi-Agent Session\n");
            system_prompt.push_str("You are in a collaborative multi-agent session.\n\n");
            system_prompt.push_str("**Participants:** ");
            system_prompt.push_str(&participants.join(", "));
            system_prompt.push_str("\n\n");
            // Agent tool usage instructions are in workspace.rs (SOUL.md "Agent Tool" section).
            // Not duplicated here to avoid token waste and inconsistency.
        }

        // L0: pinned memory chunks
        let pinned_budget = deps.pinned_budget_tokens();
        let (pinned_text, pinned_ids) = deps.build_memory_context(pinned_budget).await;
        if !pinned_text.is_empty() {
            system_prompt.push_str(&pinned_text);
        }
        deps.store_pinned_chunk_ids(pinned_ids).await;

        tracing::info!(
            agent = %deps.agent_name(),
            prompt_bytes = system_prompt.len(),
            prompt_approx_tokens = system_prompt.len() / 4,
            "system_prompt_size"
        );

        // 5. Assemble messages
        let mut messages: Vec<hydeclaw_types::Message> = vec![hydeclaw_types::Message {
            role: hydeclaw_types::MessageRole::System,
            content: system_prompt,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        }];

        for row in &history {
            let content_lower = row.content.to_lowercase();
            if content_lower.contains("heartbeat_ok")
                || content_lower.contains("heartbeat ok")
                || (content_lower.contains("nothing to announce") && content_lower.len() < 100)
            {
                continue;
            }
            messages.push(crate::agent::engine::row_to_message(row));
        }

        // Transcript repair — differential append scoped to last dangling assistant (ENG-01)
        if let Some(last_idx) = messages.iter().rposition(|m| {
            m.role == hydeclaw_types::MessageRole::Assistant
                && m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
        }) {
            let has_results = messages[last_idx + 1..]
                .iter()
                .any(|m| m.role == hydeclaw_types::MessageRole::Tool);
            if !has_results {
                let all_call_ids: Vec<String> = messages[last_idx]
                    .tool_calls
                    .as_ref()
                    .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
                    .unwrap_or_default();

                let existing_ids: std::collections::HashSet<&str> = messages[last_idx + 1..]
                    .iter()
                    .filter(|m| m.role == hydeclaw_types::MessageRole::Tool)
                    .filter_map(|m| m.tool_call_id.as_deref())
                    .collect();
                let missing_ids: Vec<String> = all_call_ids
                    .into_iter()
                    .filter(|id| !existing_ids.contains(id.as_str()))
                    .collect();

                if !missing_ids.is_empty() {
                    tracing::warn!(
                        session_id = %session_id,
                        count = missing_ids.len(),
                        "dangling tool calls detected — inserting synthetic results"
                    );

                    if let Err(e) = deps
                        .session_insert_missing_tool_results(session_id, &missing_ids)
                        .await
                    {
                        tracing::warn!(error = %e, "failed to insert synthetic tool results");
                    }

                    for call_id in missing_ids {
                        messages.push(hydeclaw_types::Message {
                            role: hydeclaw_types::MessageRole::Tool,
                            content: "[interrupted] Tool execution was interrupted (process restart). Result unavailable.".to_string(),
                            tool_calls: None,
                            tool_call_id: Some(call_id),
                            thinking_blocks: vec![],
                        });
                    }
                }
            }
        }

        // Sanitize MiniMax XML tool calls
        if messages
            .iter()
            .any(|m| m.role == hydeclaw_types::MessageRole::Tool && m.content.contains("<minimax:tool_call>"))
        {
            messages = messages
                .into_iter()
                .map(|mut m| {
                    if m.role == hydeclaw_types::MessageRole::Tool {
                        m.content = strip_minimax_xml(&m.content);
                    }
                    m
                })
                .collect();
            tracing::warn!("sanitized MiniMax XML tool calls from session context");
        }

        // Proactive tool output pruning
        if let Some(keep_turns) = deps.agent_prune_tool_output_after_turns()
            && keep_turns > 0
        {
            messages = prune_old_tool_outputs(&messages, keep_turns);
            tracing::debug!(keep_turns, "proactive tool output pruning applied");
        }

        // 6. Available tools (if requested)
        let tools = if include_tools {
            let mut tool_list = deps.internal_tool_definitions();

            // Shared YAML tools (cached)
            let yaml_tools = deps.load_yaml_tools_cached().await;
            let is_base = deps.agent_base();
            let penalties = deps.tool_penalties().await;
            let mut yaml_filtered: Vec<_> = yaml_tools
                .into_iter()
                .filter(|t| !t.required_base || is_base)
                .collect();
            yaml_filtered.sort_by(|a, b| {
                let pa = penalties.get(&a.name).copied().unwrap_or(1.0);
                let pb = penalties.get(&b.name).copied().unwrap_or(1.0);
                pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
            });
            tool_list.extend(yaml_filtered.into_iter().map(|t| t.to_tool_definition()));

            // MCP tools
            tool_list.extend(deps.mcp_tool_definitions().await);

            let mut all_tools = deps.filter_tools_by_policy(tool_list);

            // Dynamic top-K
            if let Some(max_k) = deps.agent_max_tools_in_context()
                && all_tools.len() > max_k
                && !user_text.is_empty()
            {
                all_tools = deps
                    .select_top_k_tools_semantic(all_tools, &user_text, max_k)
                    .await;
            }

            all_tools
        } else {
            vec![]
        };

        Ok(ContextSnapshot {
            session_id,
            messages,
            tools,
        })
    }
}

// ── MockContextBuilder (test-only) ────────────────────────────────────────────

#[cfg(test)]
pub mod mock {
    use super::*;

    /// Test-only mock — returns a canned `ContextSnapshot` without DB, filesystem, or LLM.
    /// Follows the same pattern as `MockMemoryService` in memory_service.rs.
    pub struct MockContextBuilder {
        pub session_id: Uuid,
        pub messages: Vec<Message>,
        pub tools: Vec<ToolDefinition>,
    }

    impl MockContextBuilder {
        /// Create a mock that returns a minimal empty snapshot on every `build()` call.
        #[allow(dead_code)]
        pub fn new() -> Self {
            Self {
                session_id: Uuid::new_v4(),
                messages: vec![],
                tools: vec![],
            }
        }

        /// Create a mock with specific canned data.
        pub fn with_snapshot(
            session_id: Uuid,
            messages: Vec<Message>,
            tools: Vec<ToolDefinition>,
        ) -> Self {
            Self { session_id, messages, tools }
        }
    }

    #[async_trait]
    impl ContextBuilder for MockContextBuilder {
        async fn build(
            &self,
            _msg: &IncomingMessage,
            _include_tools: bool,
            _resume_session_id: Option<Uuid>,
            _force_new_session: bool,
        ) -> Result<ContextSnapshot> {
            Ok(ContextSnapshot {
                session_id: self.session_id,
                messages: self.messages.clone(),
                tools: self.tools.clone(),
            })
        }
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Strip `<minimax:tool_call>…</minimax:tool_call>` blocks from a string.
// Called from DefaultContextBuilder::build() via ContextBuilder trait object dispatch.
#[allow(dead_code)]
fn strip_minimax_xml(s: &str) -> String {
    const OPEN: &str = "<minimax:tool_call>";
    const CLOSE: &str = "</minimax:tool_call>";
    if !s.contains(OPEN) {
        return s.to_string();
    }
    let mut result = String::new();
    let mut rest = s;
    loop {
        match rest.find(OPEN) {
            None => {
                result.push_str(rest);
                break;
            }
            Some(start) => {
                result.push_str(&rest[..start]);
                let after = &rest[start + OPEN.len()..];
                rest = match after.find(CLOSE) {
                    Some(end) => &after[end + CLOSE.len()..],
                    None => break,
                };
            }
        }
    }
    result.trim().to_string()
}

/// Proactively strip tool result content from old turns to reduce LLM context on load.
// Called from DefaultContextBuilder::build() via ContextBuilder trait object dispatch.
#[allow(dead_code)]
fn prune_old_tool_outputs(messages: &[hydeclaw_types::Message], keep_turns: usize) -> Vec<hydeclaw_types::Message> {
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == hydeclaw_types::MessageRole::User)
        .map(|(i, _)| i)
        .collect();

    if user_indices.len() <= keep_turns {
        return messages.to_vec();
    }

    let cutoff = user_indices[user_indices.len() - keep_turns];

    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i < cutoff && m.role == hydeclaw_types::MessageRole::Tool && !m.content.is_empty() {
                let n = m.content.len();
                hydeclaw_types::Message {
                    content: format!("[output omitted, {n} chars]"),
                    ..m.clone()
                }
            } else {
                m.clone()
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::mock::MockContextBuilder;
    use super::*;
    use chrono::Utc;

    fn make_incoming_message() -> IncomingMessage {
        IncomingMessage {
            user_id: "test-user".to_string(),
            context: serde_json::Value::Null,
            text: Some("hello".to_string()),
            attachments: vec![],
            agent_id: "test-agent".to_string(),
            channel: "test-channel".to_string(),
            timestamp: Utc::now(),
            formatting_prompt: None,
            tool_policy_override: None,
            leaf_message_id: None,
            user_message_id: None,
        }
    }

    #[tokio::test]
    async fn mock_context_builder_returns_canned_snapshot() {
        let sid = Uuid::new_v4();
        let mock = MockContextBuilder::with_snapshot(sid, vec![], vec![]);

        let msg = make_incoming_message();
        let snap = mock.build(&msg, true, None, false).await.unwrap();

        assert_eq!(snap.session_id, sid);
        assert!(snap.messages.is_empty());
        assert!(snap.tools.is_empty());
    }

    #[tokio::test]
    async fn mock_context_builder_with_messages() {
        let sid = Uuid::new_v4();
        let msgs = vec![hydeclaw_types::Message {
            role: hydeclaw_types::MessageRole::System,
            content: "You are a test agent.".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        }];
        let mock = MockContextBuilder::with_snapshot(sid, msgs, vec![]);

        let msg = make_incoming_message();
        let snap = mock.build(&msg, false, None, false).await.unwrap();

        assert_eq!(snap.messages.len(), 1);
        assert_eq!(snap.messages[0].content, "You are a test agent.");
    }

    #[test]
    fn mock_context_builder_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockContextBuilder>();
    }
}
