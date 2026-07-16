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
pub struct AgentDetailSkillReviewDto {
    pub enabled: bool,
    pub min_tool_calls: u32,
}
crate::register_ts_dto!(AgentDetailSkillReviewDto);

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
pub struct AgentDetailSoulDto {
    pub enabled: bool,
    pub reflection_threshold: f64,
    // u64 → number: values always within JS safe integer range (minutes)
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub reflection_cooldown_minutes: u64,
    pub context_top_k: usize,
    pub context_budget_tokens: u32,
    pub max_events_per_session: usize,
}
crate::register_ts_dto!(AgentDetailSoulDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailDriftDto {
    pub enabled: bool,
    pub threshold: f32,
    pub min_history: usize,
    pub baseline_turns: usize,
    pub correct: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub anchor: Option<String>,
}
crate::register_ts_dto!(AgentDetailDriftDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailInitiativeDto {
    pub enabled: bool,
    pub daily_proposal_cap: u32,
    pub decompose: bool,
    pub daily_plan: bool,
    pub auto_approve_day_plan: bool,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub daily_token_budget: u64,
}
crate::register_ts_dto!(AgentDetailInitiativeDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailEmotionDto {
    pub enabled: bool,
    pub intensity_importance_k: f32,
    pub blend_rate: f32,
    pub decay_half_life_hours: f32,
}
crate::register_ts_dto!(AgentDetailEmotionDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailToolDispatcherDto {
    pub enabled: bool,
    pub core_extra: Vec<String>,
    pub promotion_max: u32,
}
crate::register_ts_dto!(AgentDetailToolDispatcherDto);

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
pub struct WebhookDto {
    pub url: String,
    pub events: Vec<String>,
    pub mode: String,           // "async" | "decision"
    pub tool_matcher: Option<String>,
    pub on_failure: String,     // "open" | "closed"
    // u64 → number: timeout values always within JS safe integer range
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub timeout_ms: u64,
    pub allow_internal: bool,
}
crate::register_ts_dto!(WebhookDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailHooksDto {
    pub log_all_tool_calls: bool,
    pub block_tools: Vec<String>,
    pub webhooks: Vec<WebhookDto>,
}
crate::register_ts_dto!(AgentDetailHooksDto);

/// Computed (never stored directly) view of which capability slots an agent's
/// profile has at least one provider entry for. A capability is `true` when
/// the resolved `Slots` map has a non-empty vector for that key — see
/// `dto.rs::capabilities_from_slots`. The UI gates feature affordances
/// (voice input, TTS playback, image generation, etc.) on this object instead
/// of guessing from the removed per-agent provider fields.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentCapabilitiesDto {
    pub text: bool,
    pub stt: bool,
    pub tts: bool,
    pub vision: bool,
    pub imagegen: bool,
    pub websearch: bool,
}
crate::register_ts_dto!(AgentCapabilitiesDto);

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
    /// Name of the row in the `profiles` table this agent resolves providers
    /// from (replaces the removed provider/model/provider_connection/
    /// fallback_provider/tts_provider/imagegen_provider fields).
    pub profile: String,
    pub capabilities: AgentCapabilitiesDto,
    pub temperature: f64,
    pub max_tokens: Option<u32>,
    pub access: Option<AgentDetailAccessDto>,
    pub heartbeat: Option<AgentDetailHeartbeatDto>,
    pub tools: Option<AgentDetailToolsDto>,
    pub compaction: Option<AgentDetailCompactionDto>,
    pub skill_review: Option<AgentDetailSkillReviewDto>,
    pub session: Option<AgentDetailSessionDto>,
    /// Pre-signed URL for the icon under `/api/uploads/{id}`. Long-TTL
    /// (`HISTORICAL_URL_TTL_SECS`) so a saved agent icon stays viewable across
    /// restarts. `None` when the agent has no icon in the `uploads` table or
    /// no upload key is available.
    pub icon_url: Option<String>,
    pub max_tools_in_context: Option<usize>,
    pub tool_loop: Option<AgentDetailToolLoopDto>,
    pub tool_dispatcher: Option<AgentDetailToolDispatcherDto>,
    pub soul: AgentDetailSoulDto,
    pub drift: AgentDetailDriftDto,
    pub initiative: AgentDetailInitiativeDto,
    pub emotion: AgentDetailEmotionDto,
    pub approval: Option<AgentDetailApprovalDto>,
    pub routing: Vec<AgentDetailRoutingDto>,
    pub watchdog: Option<AgentDetailWatchdogDto>,
    pub hooks: Option<AgentDetailHooksDto>,
    pub max_history_messages: Option<usize>,
    // u64 → number: token budgets always within JS safe integer range
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub daily_budget_tokens: u64,
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
    /// See `AgentDetailDto::profile`.
    pub profile: String,
    pub capabilities: AgentCapabilitiesDto,
    /// Pre-signed URL for the icon (see `AgentDetailDto::icon_url`).
    pub icon_url: Option<String>,
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
