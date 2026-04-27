// DTO struct definitions for AgentDetail — no crate-internal imports.
//
// Extracted as a leaf module so that `lib.rs` can re-export these types
// under the `ts-gen` feature without cascading the `config`/`memory`/etc.
// module trees into the lib facade. This file is `include!`-d by `dto.rs`
// and re-exported by `lib.rs` under `#[cfg(feature = "ts-gen")]`.

use serde::Serialize;

// ── Nested DTOs ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailAccessDto {
    pub mode: String,
    pub owner_id: Option<String>,
}
crate::register_ts_dto!(AgentDetailAccessDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailHeartbeatDto {
    pub cron: String,
    pub timezone: Option<String>,
    pub announce_to: Option<String>,
}
crate::register_ts_dto!(AgentDetailHeartbeatDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailToolGroupsDto {
    pub git: bool,
    pub tool_management: bool,
    pub skill_editing: bool,
    pub session_tools: bool,
}
crate::register_ts_dto!(AgentDetailToolGroupsDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailToolsDto {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub allow_all: bool,
    pub deny_all_others: bool,
    pub groups: AgentDetailToolGroupsDto,
}
crate::register_ts_dto!(AgentDetailToolsDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailCompactionDto {
    pub enabled: bool,
    pub threshold: f64,
    pub preserve_tool_calls: bool,
    pub preserve_last_n: u32,
    pub max_context_tokens: Option<u32>,
}
crate::register_ts_dto!(AgentDetailCompactionDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailSessionDto {
    pub dm_scope: String,
    pub ttl_days: u32,
    pub max_messages: u32,
    pub prune_tool_output_after_turns: Option<usize>,
}
crate::register_ts_dto!(AgentDetailSessionDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailToolLoopDto {
    pub max_iterations: usize,
    pub compact_on_overflow: bool,
    pub detect_loops: bool,
    pub warn_threshold: usize,
    pub break_threshold: usize,
    pub max_consecutive_failures: usize,
    pub max_auto_continues: u8,
    pub max_loop_nudges: usize,
    pub ngram_cycle_length: usize,
    // error_break_threshold is intentionally absent — internal executor hint, not exposed via API
}
crate::register_ts_dto!(AgentDetailToolLoopDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailApprovalDto {
    pub enabled: bool,
    pub require_for: Vec<String>,
    pub require_for_categories: Vec<String>,
    // u64 → number: values are always within JS safe integer range for timeouts
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub timeout_seconds: u64,
}
crate::register_ts_dto!(AgentDetailApprovalDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailRoutingDto {
    pub condition: String,
    pub connection: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    // u64 → number: values are always within JS safe integer range for cooldown
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub cooldown_secs: u64,
}
crate::register_ts_dto!(AgentDetailRoutingDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailWatchdogDto {
    // u64 → number: values are always within JS safe integer range for inactivity
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub inactivity_secs: u64,
}
crate::register_ts_dto!(AgentDetailWatchdogDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailHooksDto {
    pub log_all_tool_calls: bool,
    pub block_tools: Vec<String>,
}
crate::register_ts_dto!(AgentDetailHooksDto);

// ── Top-level DTO ───────────────────────────────────────────────────────────

/// Response shape for GET /api/agents/{name}.
/// Field order matches the json!{} literal in schema.rs for diff readability.
/// No skip_serializing_if on Option fields (must emit null to match original shape).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailDto {
    pub name: String,
    pub language: String,
    pub provider: String,
    pub model: String,
    pub provider_connection: Option<String>,
    pub fallback_provider: Option<String>,
    pub temperature: f64,
    pub max_tokens: Option<u32>,
    pub access: Option<AgentDetailAccessDto>,
    pub heartbeat: Option<AgentDetailHeartbeatDto>,
    pub tools: Option<AgentDetailToolsDto>,
    pub compaction: Option<AgentDetailCompactionDto>,
    pub session: Option<AgentDetailSessionDto>,
    pub icon: Option<String>,
    pub max_tools_in_context: Option<usize>,
    pub tool_loop: Option<AgentDetailToolLoopDto>,
    pub approval: Option<AgentDetailApprovalDto>,
    pub routing: Vec<AgentDetailRoutingDto>,
    pub watchdog: Option<AgentDetailWatchdogDto>,
    pub hooks: Option<AgentDetailHooksDto>,
    pub max_history_messages: Option<usize>,
    // u64 → number: token budgets always within JS safe integer range
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub daily_budget_tokens: u64,
    pub max_agent_turns: Option<usize>,
    pub max_failover_attempts: u32,
    pub is_running: bool,
    pub config_dirty: bool,
    /// Injected by the handler from scoped TTS_VOICE secret; absent when not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub voice: Option<String>,
}
crate::register_ts_dto!(AgentDetailDto);

// ── AgentInfo DTOs (GET /api/agents) ────────────────────────────────────────

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentInfoToolPolicyDto {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub allow_all: bool,
}
crate::register_ts_dto!(AgentInfoToolPolicyDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentInfoDto {
    pub name: String,
    pub language: String,
    pub model: String,
    pub provider: String,
    pub provider_connection: Option<String>,
    pub fallback_provider: Option<String>,
    pub icon: Option<String>,
    pub temperature: f64,
    pub has_access: bool,
    pub access_mode: Option<String>,
    pub has_heartbeat: bool,
    pub heartbeat_cron: Option<String>,
    pub heartbeat_timezone: Option<String>,
    pub tool_policy: Option<AgentInfoToolPolicyDto>,
    pub routing_count: usize,
    pub is_running: bool,
    pub config_dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub base: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub pending_delete: Option<bool>,
}
crate::register_ts_dto!(AgentInfoDto);
