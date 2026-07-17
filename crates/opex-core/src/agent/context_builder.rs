//! `ContextBuilder` trait, `ContextSnapshot` return type, and `DefaultContextBuilder` implementation.
//!
//! Decoupled from the engine for testability — `MockContextBuilder` can be injected in tests.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use opex_types::{IncomingMessage, Message, ToolDefinition};
use std::sync::Weak;
use uuid::Uuid;

/// Process-wide guard: warn at most once per process about `always_core`
/// names that match no tool in the assembled universe (F3).
static ALWAYS_CORE_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

// ── Public types ──────────────────────────────────────────────────────────────

/// Estimate-only per-category context-size breakdown (T17 triage — hermes
/// parity for the `/usage` popover). Every value is a chars/4 heuristic, the
/// same approximation already used for the aggregate `prompt_approx_tokens`
/// / `tools_tokens` log fields below — NOT a provider-measured token count.
/// Categories are additive (sum ≈ total estimated prompt size) but are not
/// guaranteed to exactly reconcile with the provider's real prompt_tokens.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ContextBreakdown {
    /// Base workspace prompt + capability/runtime header + MCP schema list
    /// (the `build_system_prompt(...)` output), before any of the
    /// request-specific blocks below are appended.
    pub system_prompt: usize,
    /// Skill-capture hint + skill-trigger hint + tool-trigger hint blocks
    /// appended conditionally per-request.
    pub skills: usize,
    /// Multi-agent participants block.
    pub multi_agent: usize,
    /// Pinned memory chunks (L0 budget) injected into the system prompt.
    pub memory: usize,
    /// Soul context blocks: SELF portrait re-serialization + L1 "biography"
    /// block (autobiographical memory retrieved by relevance). Spec §4/§6.
    pub soul: usize,
    /// Session TODO list block.
    pub todo: usize,
    /// Serialized tool definitions sent to the provider (builtin + YAML +
    /// capability + MCP, after policy filtering / dispatcher partitioning).
    pub tools: usize,
    /// Conversation history (loaded messages, pre-repair).
    pub conversation: usize,
}

impl ContextBreakdown {
    #[allow(dead_code)] // consumed by the removed context-breakdown endpoint's aggregation.
    pub fn total(&self) -> usize {
        self.system_prompt + self.skills + self.multi_agent + self.memory + self.soul + self.todo + self.tools + self.conversation
    }
}

/// Named return type for context building — replaces the anonymous
/// `(Uuid, Vec<Message>, Vec<ToolDefinition>)` tuple.
#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    pub session_id: Uuid,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    /// How this session is being entered. Threaded through to bootstrap so
    /// it can decide on `LoopDetector` warm-up and pick the correct
    /// `claim_session_for_reentry` mode.
    pub reentry_mode: opex_db::ReentryMode,
    /// CACHE-02: per-agent CLAUDE.md content for the third cache
    /// breakpoint. `Some(text)` only when `is_base && prompt_cache &&
    /// non-empty file`. Threaded through `BootstrapOutcome` and consumed
    /// at every `CallOptions` site in `pipeline::execute`.
    pub claude_md_content: Option<String>,
    /// T17 triage: estimate-only per-category context-size breakdown, used by
    /// `GET /api/agents/{name}/context-breakdown`. Cached on `AgentState` by
    /// the caller (not persisted) — recomputed on every `build_context` call.
    pub breakdown: ContextBreakdown,
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
        chat_scope: Option<&str>,
    ) -> Result<(Uuid, opex_db::ReentryMode)>;
    /// Read-only `run_status` lookup. Used by `DefaultContextBuilder::build`
    /// when classifying `resume_session_id` paths so a UI-explicit reopen
    /// of a `failed`/`interrupted` session uses `ReentryMode::ExplicitResume`
    /// rather than mis-classifying as `ResumeRunning`.
    async fn session_get_run_status(&self, sid: Uuid) -> Result<Option<String>>;
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
    /// CACHE-02: whether the agent has `prompt_cache = true` in its TOML.
    /// Used by `DefaultContextBuilder::build` to decide whether to load
    /// CLAUDE.md as a separate cache breakpoint block.
    fn agent_prompt_cache(&self) -> bool;
    fn agent_language(&self) -> &str;
    fn agent_max_history_messages(&self) -> i64;
    fn agent_dm_scope(&self) -> &str;
    fn agent_prune_tool_output_after_turns(&self) -> Option<usize>;
    fn agent_max_tools_in_context(&self) -> Option<usize>;

    // Workspace
    async fn load_workspace_prompt(&self) -> Result<String>;
    /// CACHE-02: load `workspace/agents/{Name}/CLAUDE.md` as a standalone
    /// string. `Ok(None)` for missing or whitespace-only files. Only
    /// invoked by the cache-aware build path.
    async fn load_claude_md(&self) -> Result<Option<String>>;
    /// CACHE-02: load the per-agent system prompt EXCLUDING CLAUDE.md.
    /// Used by the cache-aware path so CLAUDE.md is emitted as its own
    /// breakpoint block (via `load_claude_md`) rather than inlined into
    /// the monolithic prompt.
    async fn load_workspace_prompt_excluding_claude_md(&self) -> Result<String>;

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
    /// Render this session's TODO list as a context block, or `None` if empty.
    async fn session_todo_block(&self, session_id: Uuid) -> Option<String>;

    /// Soul context blocks (spec §4/§6): (SELF portrait, L1 biography).
    /// (None, None) when [agent.soul] is disabled. Fail-soft inside.
    async fn soul_blocks(&self, user_text: &str, session_id: Uuid) -> (Option<String>, Option<String>);

    /// Persona-drift probe (Stage B). Returns the identity-anchor block to append
    /// to the system prompt when correction fires (`[agent.drift] correct=true`
    /// and z-score fires per Schmitt hysteresis), else `None`. Detect-only + timeline logging are
    /// unchanged; the return is `None` on every non-correcting path.
    async fn drift_probe(&self, history: &[opex_db::sessions::MessageRow], session_id: Uuid) -> Option<String>;

    /// Stage C: read-only «current focus + active initiative goals» block.
    /// Framed + sanitized; None when nothing to show or initiative disabled.
    async fn initiative_block(&self, agent: &str) -> Option<String>;

    // Workspace
    fn workspace_dir(&self) -> &str;

    // Database
    fn db(&self) -> sqlx::PgPool;

    /// Agent's resolved profile slots (capability -> ordered provider list).
    /// Gates capability tools (`capability_tool_defs`) and the dispatcher's
    /// extension-tool lookup — no `provider_active` DB query needed.
    fn profile_slots(&self) -> &crate::db::profiles::Slots;

    // Tools
    fn internal_tool_definitions(&self) -> Vec<ToolDefinition>;
    async fn capability_tool_defs(&self) -> Vec<crate::tools::yaml_tools::YamlToolDef>;
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

    // Dispatcher-related accessors consumed by `DefaultContextBuilder::build`.
    /// Whether the dispatcher is enabled for this agent.
    fn agent_tool_dispatcher_enabled(&self) -> bool;

    /// Names the operator wants kept in the per-turn core array regardless
    /// of dispatcher partition. Subject to deny + base + existence filters
    /// at apply time.
    fn agent_core_extra(&self) -> &[String];

    /// Global `[tool_dispatcher] always_core` list — extension tools promoted
    /// to native tools[] for every dispatcher-mode agent (and excluded from the
    /// dispatcher catalogue/hint/suppressor).
    fn dispatcher_always_core(&self) -> &[String];

    /// Agent's effective tool-policy deny list (consumed by trigger-hint
    /// logic and extension-list assembly). Returns the union of
    /// `agent.tools.deny` and the delegation-computed deny list
    /// (`SUBAGENT_DENIED_TOOLS` + `blocked_tools_extra`).
    /// Returns an empty Vec when no policy is set and delegation defaults are empty.
    fn cfg_deny_list(&self) -> Vec<String>;

    /// Optional MCP registry for tool discovery (consumed by extension-list
    /// build). Returns `None` when MCP is not configured for this agent.
    fn mcp_registry(&self) -> Option<&crate::mcp::McpRegistry>;
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
        let (session_id, reentry_mode) = if let Some(sid) = resume_session_id {
            let id = deps.session_resume(sid).await?;
            // Classify based on actual current status. UI users explicitly
            // opening a failed/interrupted session should be ALLOWED to
            // continue (their choice). Soft-terminal → ExplicitResume so
            // claim_session_for_reentry skips the strict per-mode WHERE.
            let status_str = deps.session_get_run_status(id).await?;
            let parsed = status_str.as_deref().and_then(opex_db::SessionStatus::parse);
            let mode = match parsed {
                None => opex_db::ReentryMode::NewSession,
                Some(opex_db::SessionStatus::Running) => opex_db::ReentryMode::ResumeRunning,
                Some(opex_db::SessionStatus::Done) => opex_db::ReentryMode::NewTurnAfterDone,
                Some(_) => opex_db::ReentryMode::ExplicitResume,
            };
            (id, mode)
        } else if force_new_session {
            let id = deps.session_create_new(&msg.user_id, &msg.channel).await?;
            (id, opex_db::ReentryMode::NewSession)
        } else {
            let dm_scope = deps.agent_dm_scope().to_string();
            let chat_scope = msg.chat_scope();
            deps.session_get_or_create(&msg.user_id, &msg.channel, &dm_scope, chat_scope.as_deref()).await?
        };

        // 2. Load conversation history (branch-aware when leaf_message_id is set)
        let history = if let Some(leaf_id) = msg.leaf_message_id {
            deps.session_load_branch_messages(session_id, leaf_id).await?
        } else {
            let limit = deps.agent_max_history_messages();
            deps.session_load_messages(session_id, limit).await?
        };

        // Stage B: persona-drift probe (detect+log, fail-soft; correction anchor
        // injected at the tail of the system prompt below when it fires).
        let drift_anchor = deps.drift_probe(&history, session_id).await;

        // T17: conversation-history size estimate (chars/4 heuristic, same as
        // the system-prompt estimate below) — captured pre-repair, before any
        // synthetic/orphan-result adjustments below.
        let conversation_chars: usize = history.iter().map(|row| row.content.len()).sum();

        // 3. Build system prompt with MCP tool schemas
        // CACHE-02 / Pitfall 5: only base agents with prompt_cache get
        // CLAUDE.md as a separate breakpoint block. All other paths use the
        // monolithic prompt (CLAUDE.md inlined if present).
        let use_third_breakpoint = deps.agent_base() && deps.agent_prompt_cache();
        let (ws_prompt, claude_md_content): (String, Option<String>) = if use_third_breakpoint {
            let prompt = deps.load_workspace_prompt_excluding_claude_md().await?;
            let claude = deps.load_claude_md().await?;
            (prompt, claude)
        } else {
            let prompt = deps.load_workspace_prompt().await?;
            (prompt, None)
        };

        // MCP tool schemas in system prompt: name + description only.
        let mcp_defs = deps.mcp_tool_definitions().await;
        let mcp_schemas: Vec<String> = mcp_defs
            .iter()
            .map(|t| format!("- **{}**: {}", t.name, t.description))
            .collect();

        // 4. Capabilities + system prompt
        let user_text = msg.text.clone().unwrap_or_default();

        let capabilities = crate::agent::workspace::CapabilityFlags {
            has_search: deps.has_tool("search_web").await,
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

        // Dispatcher partition gating: when enabled, the LLM only sees a small
        // tools array (static core ∪ core_extra ∪ promoted) and a catalogue
        // hint pointing it at the `tool_use` discovery flow.
        let dispatcher_enabled = deps.agent_tool_dispatcher_enabled();
        let extension_catalogue = if dispatcher_enabled {
            Some(
                "These tools are not preloaded. To use them: search → describe → call.\n\n\
                 Categories: agent management, scheduling, secrets, system services, \
                 channel actions, git operations, browser, canvas, rich cards, YAML tools, \
                 MCP tools.\n\n\
                 Workflow:\n\
                 1. tool_use(action=\"search\", query=\"<keywords>\") — discover relevant tools\n\
                 2. tool_use(action=\"describe\", name=\"<tool>\") — read full input schema\n\
                 3. tool_use(action=\"call\", name=\"<tool>\", arguments={...}) — invoke\n\n\
                 Tip: search by intent (\"send notification\", \"schedule task\"), not exact names.\n"
                    .to_string(),
            )
        } else {
            None
        };

        let mut runtime = deps.runtime_context(msg);
        runtime.channels = deps.get_channel_info().await;
        let mut system_prompt = crate::agent::workspace::build_system_prompt(
            &ws_prompt,
            &mcp_schemas,
            &capabilities,
            deps.agent_language(),
            &runtime,
            extension_catalogue.as_deref(),
        );

        // Soul: SELF portrait — сразу после workspace-промпта (spec §4)
        let (self_block, l1_block) = deps.soul_blocks(&user_text, session_id).await;
        let pre_self_len = system_prompt.len();
        if let Some(b) = self_block {
            system_prompt.push_str(&b);
        }
        let self_len = system_prompt.len() - pre_self_len;

        // Stage C: read-only «current focus + active initiative goals» block —
        // sits right after the soul SELF block, before any request-specific ones.
        if let Some(block) = deps.initiative_block(deps.agent_name()).await {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&block);
        }

        // T17: base prompt size before any request-specific blocks are appended.
        let base_prompt_len = system_prompt.len();

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
                // Fire-and-forget: reactivate archived skills or track last_used_at
                {
                    let skill_name = skill.meta.name.clone();
                    let skill_state = skill.meta.state.clone();
                    let workspace = deps.workspace_dir().to_string();
                    let now_iso = chrono::Utc::now().to_rfc3339();

                    if matches!(skill_state, crate::skills::SkillState::Archived) {
                        let db = deps.db();
                        let agent_name = deps.agent_name().to_string();
                        // AUDIT-FF-009: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
                        tokio::spawn(async move {
                            crate::skills::reactivate_skill(
                                &workspace,
                                &skill_name,
                                &db,
                                &agent_name,
                                &now_iso,
                            ).await;
                        });
                    } else {
                        // AUDIT-FF-010: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
                        tokio::spawn(async move {
                            let safe_name = skill_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
                            let skill_path = format!("{}/skills/{}.md", workspace, safe_name);
                            // Only update workspace/skills/ — config/skills/ have a different on-disk
                            // location and should not be tracked.
                            if tokio::fs::metadata(&skill_path).await.is_ok() {
                                crate::skills::update_skill_last_used_if_stale(
                                    &skill_path,
                                    &now_iso,
                                    chrono::Duration::hours(1),
                                ).await;
                            }
                        });
                    }
                }
            }
        }

        // Tool trigger hint — top-1 extension match.
        // Both `cfg_deny_list()` and `mcp_registry()` were added to
        // ContextBuilderDeps in Tasks 14/15.
        if dispatcher_enabled && !user_text.is_empty() {
            let deny: Vec<String> = deps.cfg_deny_list();

            let candidates = crate::agent::dispatcher::build_extension_tool_list(
                deps.agent_base(),
                &deny,
                &std::collections::HashSet::new(),
                deps.dispatcher_always_core(),
                deps.workspace_dir(),
                deps.profile_slots(),
                deps.mcp_registry(),
            ).await;

            if !candidates.is_empty() {
                let top1 = deps.select_top_k_tools_semantic(
                    candidates, &user_text, 1,
                ).await;
                if let Some(t) = top1.first()
                    && shares_significant_token(&user_text, &t.name, &t.description)
                {
                    system_prompt.push_str(&format!(
                        "\n\n## Relevant Tool Hint\n\
                         Your task may need an extension tool named `{}`: {}{}.\n\
                         This tool is NOT directly callable and has no function of its own — \
                         it is only reachable through the `tool_use` dispatcher. Do NOT write \
                         `{}` (or `<{}>`, `{}(...)`, `{}` followed by JSON) as message content; \
                         that is not a tool call and will be shown to the user as plain text.\n\
                         The ONLY way to use it: first call \
                         tool_use(action=\"describe\", name=\"{}\") to load its schema, then call \
                         tool_use(action=\"call\", name=\"{}\", arguments={{...}}).\n",
                        t.name,
                        t.description,
                        crate::agent::tool_handlers::tool_use::required_params_suffix(
                            &t.input_schema
                        ),
                        t.name,
                        t.name,
                        t.name,
                        t.name,
                        t.name,
                        t.name,
                    ));
                }
            }
        }

        // T17: everything appended since base_prompt (skill-capture + skill-trigger
        // + tool-trigger hints) attributed to the "skills" category.
        let skills_len = system_prompt.len() - base_prompt_len;
        let pre_multi_agent_len = system_prompt.len();

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
        let multi_agent_len = system_prompt.len() - pre_multi_agent_len;
        let pre_memory_len = system_prompt.len();

        // L0: pinned memory chunks
        let pinned_budget = deps.pinned_budget_tokens();
        let (pinned_text, pinned_ids) = deps.build_memory_context(pinned_budget).await;
        if !pinned_text.is_empty() {
            system_prompt.push_str(&pinned_text);
        }
        deps.store_pinned_chunk_ids(pinned_ids).await;
        let memory_len = system_prompt.len() - pre_memory_len;

        // Soul: L1 biography — после L0 pinned (spec §6)
        let pre_l1_len = system_prompt.len();
        if let Some(b) = l1_block {
            system_prompt.push_str(&b);
        }
        let soul_len = self_len + (system_prompt.len() - pre_l1_len);
        let pre_todo_len = system_prompt.len();

        // Session TODO list (persists across turns and context compaction)
        if let Some(todo_block) = deps.session_todo_block(session_id).await {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&todo_block);
        }
        let todo_len = system_prompt.len() - pre_todo_len;

        // Stage B Phase 2: identity anchor at the tail (highest salience) when
        // the drift z-score fired this turn. Rare (only when hysteresis is active).
        if let Some(block) = drift_anchor {
            system_prompt.push_str(&block);
        }

        tracing::info!(
            agent = %deps.agent_name(),
            prompt_bytes = system_prompt.len(),
            prompt_approx_tokens = system_prompt.len() / 4,
            "system_prompt_size"
        );

        // Captured before `system_prompt` is moved into the system message — used by
        // the `context_size` log emitted after `tools` is built.
        let prompt_tokens = system_prompt.len() / 4;

        // 5. Assemble messages
        let mut messages: Vec<opex_types::Message> = vec![opex_types::Message {
            role: opex_types::MessageRole::System,
            content: system_prompt,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
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

        // FIX H4: drop orphan tool results (a committed result whose parent
        // assistant row was lost in a crash — the two are persisted by
        // independent detached tasks) before any other repair, so the provider
        // never sees a tool message with no declaring assistant call.
        drop_orphan_tool_results(&mut messages);

        // Transcript repair — differential append scoped to the last dangling
        // assistant block (ENG-01). FIX C3: `repair_dangling_tool_calls` diffs
        // every tool_call_id, so PARTIAL parallel batches (some sibling results
        // committed, one orphaned by a crash) are also repaired — a previous
        // `has_results` gate skipped repair whenever any sibling result existed,
        // leaving the orphan dangling and risking a re-issued non-idempotent tool.
        let missing_tool_results = repair_dangling_tool_calls(&mut messages);
        if !missing_tool_results.is_empty() {
            tracing::warn!(
                session_id = %session_id,
                count = missing_tool_results.len(),
                "dangling tool calls detected — inserting synthetic results"
            );
            // Persist via the DB layer using owned String form — the DB layer
            // keeps Option<String> at the row boundary.
            if let Err(e) = deps
                .session_insert_missing_tool_results(session_id, &missing_tool_results)
                .await
            {
                tracing::warn!(error = %e, "failed to insert synthetic tool results");
            }
        }

        // Sanitize MiniMax XML tool calls
        if messages
            .iter()
            .any(|m| m.role == opex_types::MessageRole::Tool && m.content.contains("<minimax:tool_call>"))
        {
            messages = messages
                .into_iter()
                .map(|mut m| {
                    if m.role == opex_types::MessageRole::Tool {
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
        // TODO(dispatcher-integration-test): cover the partition branch with an
        // integration test once a richer Deps mock is available — see
        // tests/manual_smoke.md for the manual repro.
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
            // Capability-имена зарезервированы за встроенными инструментами —
            // выкинуть одноимённые YAML, чтобы не было дубля в списке LLM.
            yaml_filtered.retain(|t| !crate::agent::capability_tools::is_capability_tool(&t.name));
            tool_list.extend(yaml_filtered.into_iter().map(|t| t.to_tool_definition()));

            // Built-in capability tools (один на активную media-capability).
            tool_list.extend(
                deps.capability_tool_defs().await.into_iter().map(|t| t.to_tool_definition()),
            );

            // MCP tools
            tool_list.extend(deps.mcp_tool_definitions().await);

            let mut all_tools = deps.filter_tools_by_policy(tool_list);

            if dispatcher_enabled {
                // Partition: keep only static core ∪ core_extra ∪ promoted.
                // Everything else is reachable via the `tool_use` dispatcher,
                // which is itself part of the static core.
                let core_names: std::collections::HashSet<&str> =
                    crate::agent::pipeline::tool_defs::static_core_tool_names()
                        .iter()
                        .copied()
                        .collect();

                let core_extra: std::collections::HashSet<String> =
                    deps.agent_core_extra().iter().cloned().collect();

                let always_core = deps.dispatcher_always_core();

                // F3: warn once per process about always_core names that match
                // no tool in the assembled universe (typo / absent tool).
                if !always_core.is_empty() && ALWAYS_CORE_WARNED.get().is_none() {
                    let known: std::collections::HashSet<String> =
                        all_tools.iter().map(|t| t.name.clone()).collect();
                    let missing = unmatched_always_core(always_core, &known);
                    if !missing.is_empty() {
                        tracing::warn!(
                            missing = ?missing,
                            "[tool_dispatcher] always_core names not found in any tool source \
                             (typo or tool absent) — they will never be promoted"
                        );
                    }
                    let _ = ALWAYS_CORE_WARNED.set(());
                }

                all_tools.retain(|t| {
                    keep_in_native_partition(&t.name, &core_names, &core_extra, always_core)
                });
            } else if let Some(max_k) = deps.agent_max_tools_in_context() {
                // Legacy dynamic top-K path — only when dispatcher is OFF.
                if all_tools.len() > max_k && !user_text.is_empty() {
                    all_tools = deps
                        .select_top_k_tools_semantic(all_tools, &user_text, max_k)
                        .await;
                }
            }

            all_tools
        } else {
            vec![]
        };

        let tools_tokens = tools.iter()
            .map(|t| serde_json::to_string(t).map(|s| s.len()).unwrap_or(0))
            .sum::<usize>() / 4;
        tracing::info!(
            agent = %deps.agent_name(),
            prompt_tokens = prompt_tokens,
            tools_tokens = tools_tokens,
            dispatcher_enabled = dispatcher_enabled,
            "context_size"
        );

        // T17: estimate-only per-category breakdown, exposed via
        // GET /api/agents/{name}/context-breakdown. All values chars/4.
        let breakdown = ContextBreakdown {
            // `base_prompt_len` was captured AFTER the SELF block was appended
            // (spec §4 requires SELF right after the workspace prompt), so
            // `self_len` must be subtracted back out here to keep this
            // category limited to the workspace/capability/runtime prompt.
            system_prompt: (base_prompt_len - self_len) / 4,
            skills: skills_len / 4,
            multi_agent: multi_agent_len / 4,
            memory: memory_len / 4,
            soul: soul_len / 4,
            todo: todo_len / 4,
            tools: tools_tokens,
            conversation: conversation_chars / 4,
        };

        Ok(ContextSnapshot {
            session_id,
            messages,
            tools,
            reentry_mode,
            claude_md_content,
            breakdown,
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
        pub reentry_mode: opex_db::ReentryMode,
    }

    impl MockContextBuilder {
        /// Create a mock with specific canned data. Defaults `reentry_mode`
        /// to `NewSession` (use the field directly to override).
        pub fn with_snapshot(
            session_id: Uuid,
            messages: Vec<Message>,
            tools: Vec<ToolDefinition>,
        ) -> Self {
            Self {
                session_id,
                messages,
                tools,
                reentry_mode: opex_db::ReentryMode::NewSession,
            }
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
                reentry_mode: self.reentry_mode,
                claude_md_content: None,
                breakdown: ContextBreakdown::default(),
            })
        }
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Check whether the user message shares a non-trivial token (≥3 chars, not a stop word)
/// with the candidate tool's name or description. Used by trigger-hint logic as a
/// keyword-overlap floor on top of semantic similarity.
fn shares_significant_token(user_text: &str, tool_name: &str, tool_desc: &str) -> bool {
    let stop: &[&str] = &["the", "a", "an", "is", "to", "of", "for", "and", "or", "in", "on"];
    let user_lower = user_text.to_lowercase();
    let user_words: std::collections::HashSet<&str> = user_lower
        .split_whitespace()
        .filter(|w| w.len() >= 3 && !stop.contains(w))
        .collect();
    let combined = format!("{tool_name} {tool_desc}").to_lowercase();
    combined.split_whitespace()
        .any(|w| w.len() >= 3 && user_words.contains(w))
}

/// Whether a tool stays in the native per-turn `tools[]` array under the
/// dispatcher partition: static core, per-agent `core_extra`, or global
/// `always_core`. Everything else is reachable via the `tool_use` dispatcher.
fn keep_in_native_partition(
    name: &str,
    core_names: &std::collections::HashSet<&str>,
    core_extra: &std::collections::HashSet<String>,
    always_core: &[String],
) -> bool {
    core_names.contains(name)
        || core_extra.contains(name)
        || always_core.iter().any(|n| n == name)
}

/// `always_core` names that match no tool in `known` (the assembled tool
/// universe). Used to warn the operator about typos / absent tools.
fn unmatched_always_core(
    configured: &[String],
    known: &std::collections::HashSet<String>,
) -> Vec<String> {
    configured.iter().filter(|n| !known.contains(*n)).cloned().collect()
}

/// Strip `<minimax:tool_call>…</minimax:tool_call>` blocks from a string.
// Called from DefaultContextBuilder::build() via ContextBuilder trait object dispatch.
// reviewed: offsets from find() + ASCII marker const .len() — char boundaries
#[allow(clippy::string_slice)]
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
fn prune_old_tool_outputs(messages: &[opex_types::Message], keep_turns: usize) -> Vec<opex_types::Message> {
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == opex_types::MessageRole::User)
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
            if i < cutoff && m.role == opex_types::MessageRole::Tool && !m.content.is_empty() {
                let n = m.content.len();
                opex_types::Message {
                    content: format!("[output omitted, {n} chars]"),
                    ..m.clone()
                }
            } else {
                m.clone()
            }
        })
        .collect()
}

/// Repair a dangling assistant-with-tool-calls block by synthesising
/// `[interrupted:verify]` tool results for tool calls that have no matching result.
///
/// Operates on the LAST assistant-with-tool-calls block; appends synthetic `Tool`
/// messages in place and returns the synthesised `tool_call_id`s (owned) so the
/// caller can persist them via the DB layer.
fn repair_dangling_tool_calls(messages: &mut Vec<opex_types::Message>) -> Vec<String> {
    use opex_types::MessageRole;
    let Some(last_idx) = messages.iter().rposition(|m| {
        m.role == MessageRole::Assistant
            && m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    }) else {
        return Vec::new();
    };

    // FIX C3: diff EVERY tool_call_id unconditionally. The previous
    // `if !has_results` gate skipped repair whenever any sibling result existed,
    // so a partially-persisted parallel batch (some results committed, one
    // orphaned by a crash) left the orphan dangling. When all results are
    // present, `missing_ids` is empty and this is a no-op (hot path unaffected).
    let all_call_ids: Vec<opex_types::ids::ToolCallId> = messages[last_idx]
        .tool_calls
        .as_ref()
        .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
        .unwrap_or_default();

    let existing_ids: std::collections::HashSet<&str> = messages[last_idx + 1..]
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .filter_map(|m| m.tool_call_id.as_ref().map(|id| id.as_str()))
        .collect();

    let missing_ids: Vec<opex_types::ids::ToolCallId> = all_call_ids
        .into_iter()
        .filter(|id| !existing_ids.contains(id.as_str()))
        .collect();

    if missing_ids.is_empty() {
        return Vec::new();
    }

    let missing_ids_str: Vec<String> =
        missing_ids.iter().map(|id| id.as_str().to_string()).collect();

    for call_id in missing_ids {
        messages.push(opex_types::Message {
            role: MessageRole::Tool,
            content: crate::db::sessions::INTERRUPTED_TOOL_RESULT.to_string(),
            tool_calls: None,
            tool_call_id: Some(call_id),
            thinking_blocks: vec![],
            db_id: None,
        });
    }

    missing_ids_str
}

/// Drop tool-result messages whose `tool_call_id` is declared by NO assistant
/// message. Mirror of [`repair_dangling_tool_calls`] (which handles the opposite:
/// a declared call with no result).
///
/// FIX H4: the assistant-with-tool-calls row and each tool result are persisted
/// by independent detached tasks (deliberately, to survive parent-task
/// cancellation — see `pipeline::execute`). A crash can commit a tool-result row
/// while losing its parent assistant row. On reload such an "orphan" result has
/// no matching assistant `tool_call`, which makes the provider reject the whole
/// turn (e.g. 400). Removing it on the read path keeps the transcript valid
/// without changing the cancellation-safe detached persistence.
fn drop_orphan_tool_results(messages: &mut Vec<opex_types::Message>) {
    use opex_types::MessageRole;
    // Every tool_call_id declared by any assistant message. Owned String set so
    // the immutable borrow is released before the mutable `retain` below.
    let declared: std::collections::HashSet<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .filter_map(|m| m.tool_calls.as_ref())
        .flat_map(|tcs| tcs.iter().map(|tc| tc.id.as_str().to_string()))
        .collect();

    messages.retain(|m| {
        if m.role != MessageRole::Tool {
            return true;
        }
        // A tool result is valid only if some assistant declared its call id.
        // Tool messages without an id are left untouched (not this fix's concern).
        match m.tool_call_id.as_ref() {
            Some(id) => declared.contains(id.as_str()),
            None => true,
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::mock::MockContextBuilder;
    use super::*;
    use chrono::Utc;

    // ── FIX C3: dangling tool-call repair (partial parallel batches) ──

    fn dangling_tool_call(id: &str) -> opex_types::ToolCall {
        opex_types::ToolCall {
            id: opex_types::ids::ToolCallId::new(id),
            name: "noop".to_string(),
            arguments: serde_json::json!({}),
            thought_signature: None,
        }
    }

    fn repair_msg(
        role: opex_types::MessageRole,
        tool_call_id: Option<&str>,
        tool_calls: Option<Vec<opex_types::ToolCall>>,
    ) -> Message {
        Message {
            role,
            content: String::new(),
            tool_calls,
            tool_call_id: tool_call_id.map(opex_types::ids::ToolCallId::new),
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn tool_result_count(messages: &[Message]) -> usize {
        messages
            .iter()
            .filter(|m| m.role == opex_types::MessageRole::Tool)
            .count()
    }

    #[test]
    fn repair_dangling_tool_calls_repairs_partial_parallel_batch() {
        // Assistant emitted a 3-call parallel batch [A,B,C]; results for A and B
        // committed before a crash, C orphaned. The repair must synthesise ONLY C.
        let mut messages = vec![
            repair_msg(opex_types::MessageRole::User, None, None),
            repair_msg(
                opex_types::MessageRole::Assistant,
                None,
                Some(vec![
                    dangling_tool_call("A"),
                    dangling_tool_call("B"),
                    dangling_tool_call("C"),
                ]),
            ),
            repair_msg(opex_types::MessageRole::Tool, Some("A"), None),
            repair_msg(opex_types::MessageRole::Tool, Some("B"), None),
        ];

        let repaired = repair_dangling_tool_calls(&mut messages);

        assert_eq!(
            repaired,
            vec!["C".to_string()],
            "only the orphaned call C should be synthesised"
        );
        let c_results = messages
            .iter()
            .filter(|m| {
                m.role == opex_types::MessageRole::Tool
                    && m.tool_call_id.as_ref().map(|i| i.as_str()) == Some("C")
            })
            .count();
        assert_eq!(c_results, 1, "orphaned tool_call C must get exactly one synthetic result");
        assert_eq!(
            tool_result_count(&messages),
            3,
            "A + B (real) + C (synthetic) = 3 tool results, no duplicates"
        );
    }

    #[test]
    fn repair_dangling_tool_calls_noop_when_all_committed() {
        // Hot path: every tool_call already has a result → no synthetic rows.
        let mut messages = vec![
            repair_msg(opex_types::MessageRole::User, None, None),
            repair_msg(
                opex_types::MessageRole::Assistant,
                None,
                Some(vec![dangling_tool_call("A"), dangling_tool_call("B")]),
            ),
            repair_msg(opex_types::MessageRole::Tool, Some("A"), None),
            repair_msg(opex_types::MessageRole::Tool, Some("B"), None),
        ];

        let repaired = repair_dangling_tool_calls(&mut messages);

        assert!(repaired.is_empty(), "no synthetic results when every call has a result");
        assert_eq!(
            tool_result_count(&messages),
            2,
            "hot path must not add or duplicate tool results"
        );
    }

    #[test]
    fn repair_dangling_tool_calls_repairs_full_dangling_batch() {
        // Original case (zero committed results) must still repair every call.
        let mut messages = vec![
            repair_msg(opex_types::MessageRole::User, None, None),
            repair_msg(
                opex_types::MessageRole::Assistant,
                None,
                Some(vec![dangling_tool_call("A"), dangling_tool_call("B")]),
            ),
        ];

        let mut repaired = repair_dangling_tool_calls(&mut messages);
        repaired.sort();

        assert_eq!(
            repaired,
            vec!["A".to_string(), "B".to_string()],
            "all dangling calls synthesised when no results exist"
        );
        assert_eq!(tool_result_count(&messages), 2);
    }

    fn tool_result_ids(messages: &[Message]) -> Vec<&str> {
        messages
            .iter()
            .filter(|m| m.role == opex_types::MessageRole::Tool)
            .filter_map(|m| m.tool_call_id.as_ref().map(|i| i.as_str()))
            .collect()
    }

    #[test]
    fn drop_orphan_tool_results_removes_result_with_no_declaring_assistant() {
        // A crash committed a tool result for "Z" but lost its parent assistant
        // row. The orphan ("Z" declared by no assistant) must be dropped while
        // the properly-declared result "A" is kept. (FIX H4 — mirror of C3.)
        let mut messages = vec![
            repair_msg(opex_types::MessageRole::User, None, None),
            repair_msg(
                opex_types::MessageRole::Assistant,
                None,
                Some(vec![dangling_tool_call("A")]),
            ),
            repair_msg(opex_types::MessageRole::Tool, Some("A"), None),
            repair_msg(opex_types::MessageRole::Tool, Some("Z"), None),
        ];

        drop_orphan_tool_results(&mut messages);

        assert_eq!(
            tool_result_ids(&messages),
            vec!["A"],
            "orphan result Z (no declaring assistant) dropped, declared result A kept"
        );
    }

    #[test]
    fn drop_orphan_tool_results_keeps_all_declared() {
        // Hot path: every tool result has a declaring assistant → nothing dropped.
        let mut messages = vec![
            repair_msg(
                opex_types::MessageRole::Assistant,
                None,
                Some(vec![dangling_tool_call("A"), dangling_tool_call("B")]),
            ),
            repair_msg(opex_types::MessageRole::Tool, Some("A"), None),
            repair_msg(opex_types::MessageRole::Tool, Some("B"), None),
        ];

        drop_orphan_tool_results(&mut messages);

        assert_eq!(tool_result_ids(&messages), vec!["A", "B"], "declared results untouched");
    }

    #[test]
    fn repair_dangling_synthetic_text_carries_verify_tag() {
        // The synthetic result must carry the machine-readable [interrupted:verify]
        // prefix so the dispatcher (Phase 3) can require verify-before-redo for
        // non-idempotent tools. (Phase 0 tagging)
        let mut messages = vec![repair_msg(
            opex_types::MessageRole::Assistant,
            None,
            Some(vec![dangling_tool_call("A")]),
        )];

        repair_dangling_tool_calls(&mut messages);

        let synthetic = messages
            .iter()
            .find(|m| {
                m.role == opex_types::MessageRole::Tool
                    && m.tool_call_id.as_ref().map(|i| i.as_str()) == Some("A")
            })
            .expect("synthetic result for A");
        assert!(
            synthetic.content.starts_with("[interrupted:verify]"),
            "synthetic text must carry the [interrupted:verify] machine-tag, got: {}",
            synthetic.content
        );
    }

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
        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::System,
            content: "You are a test agent.".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
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

    #[test]
    fn breakdown_total_includes_soul() {
        let b = ContextBreakdown { system_prompt: 1, skills: 2, multi_agent: 3, memory: 4, soul: 7, todo: 5, tools: 6, conversation: 8 };
        assert_eq!(b.total(), 36);
    }

    #[test]
    fn native_partition_keeps_always_core() {
        use std::collections::HashSet;
        let core: HashSet<&str> = ["workspace_read"].into_iter().collect();
        let core_extra: HashSet<String> = HashSet::new();
        let always = vec!["sequentialthinking".to_string()];

        // static core kept
        assert!(super::keep_in_native_partition("workspace_read", &core, &core_extra, &always));
        // always_core kept
        assert!(super::keep_in_native_partition("sequentialthinking", &core, &core_extra, &always));
        // unrelated extension dropped
        assert!(!super::keep_in_native_partition("brave_search", &core, &core_extra, &always));
    }

    #[test]
    fn unmatched_always_core_reports_typos() {
        use std::collections::HashSet;
        let known: HashSet<String> = ["sequentialthinking".to_string()].into_iter().collect();
        let configured = vec!["sequentialthinking".to_string(), "sequentialthinkng".to_string()];
        assert_eq!(
            super::unmatched_always_core(&configured, &known),
            vec!["sequentialthinkng".to_string()],
        );
    }

}
