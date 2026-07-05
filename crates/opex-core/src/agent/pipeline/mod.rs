//! Pipeline — free functions for each step of the agent execution loop.
//!
//! Each function takes an explicit `CommandContext` parameter (see below)
//! instead of `&self` on `AgentEngine`.

/// Parameter object bundling the three decomposed AgentEngine pieces
/// plus the immutable subagent recursion depth for the current call.
pub struct CommandContext<'a> {
    pub cfg: &'a super::agent_config::AgentConfig,
    pub state: &'a super::agent_state::AgentState,
    pub tex: &'a super::tool_executor::DefaultToolExecutor,
    /// Recursion depth for subagent spawning. 0 = top-level handler,
    /// 1 = first subagent, N = N-th nested subagent.
    /// Read by `agent` tool dispatch to enforce `cfg.agent.delegation.max_depth`.
    pub subagent_depth: u8,
}

pub mod context;
pub mod llm_call;
pub mod parallel;
pub mod dispatch;
pub mod artifact_hook;
pub mod tool_defs;
pub mod chunk_truncate;
pub mod memory;
pub mod commands;
pub mod handlers;
pub mod sandbox;
pub mod subagent;
pub mod agent_tool;
pub mod sessions;
pub mod canvas;
pub mod approval;
pub mod channel_actions;
pub mod subagent_runner;
pub mod openai_compat;
pub mod cron;
pub mod sink;
pub mod finalize;
pub mod bootstrap;
pub mod behaviour;
pub mod execute;
pub mod tool_loop_helpers;
pub mod media_background;
pub mod sdk_stubs;
