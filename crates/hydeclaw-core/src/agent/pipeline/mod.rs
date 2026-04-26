//! Pipeline — free functions for each step of the agent execution loop.
//!
//! Each function takes explicit `(&AgentConfig, &AgentState, &mut RequestContext)`
//! dependencies instead of `&self` on `AgentEngine`.

/// Parameter object bundling the three decomposed AgentEngine pieces.
/// Three fields, zero methods, never grows.
pub struct CommandContext<'a> {
    pub cfg: &'a super::agent_config::AgentConfig,
    pub state: &'a super::agent_state::AgentState,
    pub tex: &'a super::tool_executor::DefaultToolExecutor,
}

pub mod context;
pub mod llm_call;
pub mod parallel;
pub mod dispatch;
pub mod artifact_hook;
pub mod tool_defs;
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
pub mod execute;
