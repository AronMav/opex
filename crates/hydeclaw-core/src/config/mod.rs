use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod validation;
pub use validation::validate_config;

/// Directory for workspace-based MCP configs: workspace/mcp/*.yaml
pub const MCP_DIR: &str = "workspace/mcp";
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AppConfig {
    pub gateway: GatewayConfig,
    #[serde(skip_serializing)]
    #[schemars(skip)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub subagents: SubagentsConfig,
    #[serde(default)]
    #[allow(dead_code)]
    pub discussion: DiscussionConfig,
    #[serde(default)]
    #[allow(dead_code)]
    #[schemars(skip)]
    pub mcp: HashMap<String, McpConfig>,
    #[serde(default)]
    #[allow(dead_code)]
    pub memory: crate::memory::MemoryConfig,
    #[serde(default)]
    #[allow(dead_code)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub docker: DockerConfig,
    /// Base URL for toolgate (STT + Vision + TTS). Defaults to service registry lookup.
    pub toolgate_url: Option<String>,
    /// Tailscale Funnel: expose gateway via Tailscale serve/funnel.
    #[serde(default)]
    pub tailscale: TailscaleConfig,
    /// OpenTelemetry trace export (requires `otel` feature).
    #[serde(default)]
    pub otel: OtelConfig,
    /// Native child processes managed by Core (channels, toolgate).
    #[serde(default, skip_serializing)]
    #[schemars(skip)]
    pub managed_process: Vec<crate::process_manager::ManagedProcessConfig>,
    /// Global LLM parameter defaults applied when agent config doesn't specify them.
    #[serde(default)]
    pub agent: AgentSectionConfig,
    /// Built-in backup scheduler (disabled by default).
    #[serde(default)]
    pub backup: BackupConfig,
    /// Phase 62 RES-03 cleanup scheduler tuning (session_events WAL retention).
    #[serde(default)]
    pub cleanup: CleanupConfig,
    /// Phase 62 RES-05 graceful-shutdown drain tuning (drain timeout).
    #[serde(default)]
    pub shutdown: ShutdownConfig,
    /// Phase 64 SEC-03 upload URL signing config.
    #[serde(default)]
    pub uploads: UploadsConfig,
}

// ── UploadsConfig (Phase 64 SEC-03) ───────────────────────────────────────────

/// Signed-URL configuration for `GET /uploads/*`.
///
/// Grace period: `require_signature=false` in v0.19.0 so existing clients that
/// fetched unsigned URLs keep working. Flip to `true` in v0.19.1.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UploadsConfig {
    /// TTL for signed `/uploads` URLs (seconds). Default: 24 h (86_400 s).
    #[serde(default = "default_signed_url_ttl")]
    pub signed_url_ttl_secs: u64,
    /// v0.19.0 grace period: accept unsigned URLs. Flip to `true` in v0.19.1
    /// to enforce HMAC verification on every request.
    #[serde(default = "default_require_signature")]
    pub require_signature: bool,
}

fn default_signed_url_ttl() -> u64 { 86_400 }
fn default_require_signature() -> bool { false }

impl Default for UploadsConfig {
    fn default() -> Self {
        Self {
            signed_url_ttl_secs: default_signed_url_ttl(),
            require_signature: default_require_signature(),
        }
    }
}

// ── BackupConfig ──────────────────────────────────────────────────────────────

/// Built-in scheduled backup configuration.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BackupConfig {
    /// Enable automatic scheduled backups (default: false).
    #[serde(default)]
    pub enabled: bool,
    /// Cron expression in 6-field tokio-cron-scheduler format (sec min hour dom mon dow).
    /// Default: "0 0 5 * * *" — daily at 05:00 UTC.
    #[serde(default = "default_backup_cron")]
    pub cron: String,
    /// Number of days to retain old backup files (default: 7).
    #[serde(default = "default_backup_retention_days")]
    pub retention_days: u32,
}

fn default_backup_cron() -> String { "0 0 5 * * *".to_string() }
fn default_backup_retention_days() -> u32 { 7 }

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cron: default_backup_cron(),
            retention_days: default_backup_retention_days(),
        }
    }
}

// ── CleanupConfig ─────────────────────────────────────────────────────────────

/// Phase 62 RES-03: batched cleanup tuning for the hourly `session_events` WAL
/// prune cron. Both fields have operator-friendly defaults; `retention_days = 0`
/// disables the hourly cleanup entirely.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CleanupConfig {
    /// Retention for `session_events` WAL rows in days. `0` disables cleanup.
    /// Default: 7 days.
    #[serde(default = "default_session_events_retention_days")]
    pub session_events_retention_days: u32,
    /// Rows deleted per batch iteration — keeps lock hold-time short and
    /// autovacuum-friendly. Must be `> 0`. Default: 5000.
    #[serde(default = "default_session_events_batch_size")]
    pub session_events_batch_size: i64,
}

fn default_session_events_retention_days() -> u32 { 7 }
fn default_session_events_batch_size() -> i64 { 5000 }

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            session_events_retention_days: default_session_events_retention_days(),
            session_events_batch_size: default_session_events_batch_size(),
        }
    }
}

// ── ShutdownConfig ────────────────────────────────────────────────────────────

/// Phase 62 RES-05: graceful shutdown drain tuning. Default
/// `drain_timeout_secs = 30`. systemd `TimeoutStopSec` must be
/// `drain_timeout_secs + 10s buffer` (40s default) so systemd never
/// SIGKILLs the process while its drain is still in progress.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ShutdownConfig {
    /// Maximum time to wait for in-flight agents to drain before forcing
    /// shutdown. Must be less than systemd `TimeoutStopSec` by at least 10
    /// seconds. Default: 30 seconds.
    #[serde(default = "default_drain_timeout_secs")]
    pub drain_timeout_secs: u64,
}

fn default_drain_timeout_secs() -> u64 { 30 }

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            drain_timeout_secs: default_drain_timeout_secs(),
        }
    }
}

/// OpenTelemetry configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[allow(dead_code)] // fields read only with `otel` feature
pub struct OtelConfig {
    /// Enable OTEL trace export. Also set `OTEL_EXPORTER_OTLP_ENDPOINT` env var.
    #[serde(default)]
    pub enabled: bool,
    /// Service name reported to the collector (default: "hydeclaw-core").
    #[serde(default = "default_otel_service")]
    pub service_name: String,
}

fn default_otel_service() -> String { "hydeclaw-core".to_string() }

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GatewayConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    pub auth_token_env: Option<String>,
    /// Base URL for externally-reachable links (e.g. uploaded media).
    /// If not set, falls back to `http://localhost:{port}`.
    pub public_url: Option<String>,
    /// Allowed CORS origins. If empty, derives from listen address.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Additional subnets whose gateway IPs should be added to auto-derived CORS origins.
    /// Useful for Docker bridge networks (e.g. ["172.17.0.0/16", "172.18.0.0/16"]).
    /// Only used when `cors_origins` is empty (auto-derivation mode).
    #[serde(default)]
    pub cors_docker_subnets: Vec<String>,
}

fn default_listen() -> String {
    "0.0.0.0:18789".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct LimitsConfig {
    #[serde(default = "default_max_requests")]
    pub max_requests_per_minute: u32,
    #[serde(default = "default_max_tool_concurrency")]
    pub max_tool_concurrency: u32,
    /// Maximum time (seconds) for a single request (LLM loop + tool calls).
    /// 0 = no limit. Default: 180 (3 minutes).
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    /// Maximum agent-to-agent turns in a single turn loop (default: 5).
    /// Exposed via GET/PUT /api/config — not consumed internally by the turn loop.
    #[serde(default = "default_max_agent_turns")]
    pub max_agent_turns: usize,
    /// Maximum characters for inter-agent context (API-only, no internal consumer).
    /// Exposed via GET/PUT /api/config — not consumed internally by the turn loop.
    #[serde(default = "default_max_inter_agent_context_chars")]
    pub max_inter_agent_context_chars: usize,
    /// Phase 64 SEC-04: cap for POST /api/restore request body size in megabytes.
    /// Default 500 MB. Enforced by `check_content_length_cap` (fast-path) +
    /// `drain_body_with_cap` (streaming byte counter). Overflow → 413 Payload Too Large.
    #[serde(default = "default_max_restore_size_mb")]
    pub max_restore_size_mb: u64,
    /// Maximum sessions retained per agent. Excess oldest non-running
    /// sessions are deleted at gateway startup and during the daily
    /// session-cleanup cron. 0 disables the cap. Default: 500.
    ///
    /// Complements `agent.session.ttl_days` (age prune): the age prune
    /// only fires at 05:00 UTC and cannot protect against high-velocity
    /// session creation (cron-heavy agents, async subagents) between runs.
    #[serde(default = "default_max_sessions_per_agent")]
    pub max_sessions_per_agent: u32,
}

fn default_max_requests() -> u32 { 300 }
fn default_max_tool_concurrency() -> u32 { 10 }
fn default_request_timeout() -> u64 { 180 }
fn default_max_agent_turns() -> usize { 5 }
fn default_max_inter_agent_context_chars() -> usize { 2000 }
fn default_max_restore_size_mb() -> u64 { 500 }
fn default_max_sessions_per_agent() -> u32 { 500 }

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_requests_per_minute: default_max_requests(),
            max_tool_concurrency: default_max_tool_concurrency(),
            request_timeout_secs: default_request_timeout(),
            max_agent_turns: default_max_agent_turns(),
            max_inter_agent_context_chars: default_max_inter_agent_context_chars(),
            max_restore_size_mb: default_max_restore_size_mb(),
            max_sessions_per_agent: default_max_sessions_per_agent(),
        }
    }
}

pub(crate) fn default_true() -> bool { true }

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[allow(dead_code)] // Deserialized from TOML; fields used for subagent configuration
pub struct SubagentsConfig {
    #[serde(default = "default_subagents_enabled")]
    pub enabled: bool,
    #[serde(default = "default_subagent_mode")]
    pub default_mode: String,
    #[serde(default = "default_max_concurrent_in_process")]
    pub max_concurrent_in_process: u32,
    #[serde(default = "default_max_concurrent_docker")]
    pub max_concurrent_docker: u32,
    #[serde(default = "default_docker_timeout")]
    pub docker_timeout: String,
    #[serde(default = "default_in_process_timeout")]
    pub in_process_timeout: String,
    pub core_image: Option<String>,
}

fn default_subagents_enabled() -> bool { true }
fn default_subagent_mode() -> String { "in-process".to_string() }
fn default_max_concurrent_in_process() -> u32 { 5 }
fn default_max_concurrent_docker() -> u32 { 3 }
fn default_docker_timeout() -> String { "5m".to_string() }
fn default_in_process_timeout() -> String { "2m".to_string() }

impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_mode: default_subagent_mode(),
            max_concurrent_in_process: default_max_concurrent_in_process(),
            max_concurrent_docker: default_max_concurrent_docker(),
            docker_timeout: default_docker_timeout(),
            in_process_timeout: default_in_process_timeout(),
            core_image: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiscussionConfig {
    /// Max discussion rounds (1-3). Research shows >3 leads to sycophancy.
    #[serde(default = "default_discussion_max_rounds")]
    pub max_rounds: u32,
    /// Per-agent timeout in seconds.
    #[serde(default = "default_discussion_agent_timeout")]
    pub agent_timeout_secs: u64,
    /// Enable response anonymization in round 2+ (reduces sycophancy).
    #[serde(default = "default_discussion_anonymize")]
    pub anonymize_after_round1: bool,
    /// Enable devil's advocate role for the last agent.
    #[serde(default = "default_discussion_advocate")]
    pub devils_advocate: bool,
    /// Enable synthesizer pass after all rounds.
    #[serde(default = "default_discussion_synthesize")]
    pub synthesize: bool,
    /// Max chars per agent response before truncation in next round.
    #[serde(default = "default_discussion_max_response_len")]
    pub max_response_len: usize,
}

fn default_discussion_max_rounds() -> u32 { 2 }
fn default_discussion_agent_timeout() -> u64 { 120 }
fn default_discussion_anonymize() -> bool { true }
fn default_discussion_advocate() -> bool { true }
fn default_discussion_synthesize() -> bool { true }
fn default_discussion_max_response_len() -> usize { 1500 }

impl Default for DiscussionConfig {
    fn default() -> Self {
        Self {
            max_rounds: default_discussion_max_rounds(),
            agent_timeout_secs: default_discussion_agent_timeout(),
            anonymize_after_round1: default_discussion_anonymize(),
            devils_advocate: default_discussion_advocate(),
            synthesize: default_discussion_synthesize(),
            max_response_len: default_discussion_max_response_len(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[allow(dead_code)] // Deserialized from TOML; protocol field reserved for MCP/HTTP routing
pub struct ToolConfig {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub url: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    pub healthcheck: Option<String>,
    pub api_key_env: Option<String>,
    pub protocol: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub ui_path: Option<String>,
}

fn default_max_concurrent() -> u32 { 5 }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[allow(dead_code)] // Deserialized from TOML; protocol field reserved for MCP/HTTP routing
pub struct McpConfig {
    /// Direct URL. If set, connects without Docker (container/port ignored).
    pub url: Option<String>,
    /// Docker container name (required when url is absent).
    pub container: Option<String>,
    /// Docker-exposed port (required when url is absent).
    pub port: Option<u16>,
    #[serde(default = "default_mcp_mode")]
    pub mode: String,
    pub idle_timeout: Option<String>,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// Whether this MCP server is enabled. Disabled servers are not loaded.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_mcp_mode() -> String { "on-demand".to_string() }
fn default_protocol() -> String { "mcp".to_string() }

/// One MCP server entry as stored in workspace/mcp/NAME.yaml.
/// Identical to `McpConfig` but includes the server name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpFileEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default = "default_mcp_mode")]
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<String>,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl McpFileEntry {
    pub fn to_config(&self) -> McpConfig {
        McpConfig {
            url: self.url.clone(),
            container: self.container.clone(),
            port: self.port,
            mode: self.mode.clone(),
            idle_timeout: self.idle_timeout.clone(),
            protocol: self.protocol.clone(),
            enabled: self.enabled,
        }
    }

}

// ── Agent config (separate TOML files) ──

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AgentConfig {
    pub agent: AgentSettings,
}

/// Fixed workspace base directory — all agents live under `workspace/{agent_name}/`.
pub const WORKSPACE_DIR: &str = "workspace";

/// Route entry under `[[agent.routing]]`. References a named DB provider
/// via `connection` — inline provider fields (provider/base_url/api_key_env/
/// api_key_envs/prompt_cache/max_tokens) are removed (spec §4.7).
///
/// Rules are evaluated in order. The first matching condition wins.
/// Supported conditions:
/// - `"default"` / `"always"` — always matches (use as catch-all / last rule)
/// - `"short"` — user message shorter than 300 chars
/// - `"long"` — user message longer than 2000 chars
/// - `"with_tools"` — LLM is given one or more tool definitions
/// - `"financial"` — message contains financial analysis keywords (RU/EN)
/// - `"analytical"` — message contains data analysis / computation keywords
/// - `"code"` — message contains programming / script keywords
/// - `"fallback"` — only used when all higher-priority providers failed
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ProviderRouteConfig {
    /// Routing condition: "default" | "short" | "long" | "fallback"
    #[serde(default = "default_condition")]
    pub condition: String,

    /// Named provider connection (required post-migration).
    #[serde(default)]
    pub connection: Option<String>,

    /// Optional model override (overrides the provider's default_model).
    pub model: Option<String>,

    /// Optional temperature override for this route.
    pub temperature: Option<f64>,

    /// Cooldown in seconds after a failover-worthy error on this route.
    /// Minimum 1 (enforced by `validate`).
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
}

impl ProviderRouteConfig {
    #[allow(dead_code)] // consumed by Task 18 routing loader; also exercised by tests below
    pub fn validate(&self) -> Result<(), String> {
        if self.cooldown_secs == 0 {
            return Err("cooldown_secs must be >= 1".into());
        }
        Ok(())
    }
}

fn default_cooldown_secs() -> u64 { 60 }

fn default_condition() -> String { "default".to_string() }

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AgentSettings {
    pub name: String,
    #[serde(default = "default_language")]
    pub language: String,
    pub provider: String,
    pub model: String,
    /// Named LLM provider connection (from providers table).
    /// If set, overrides `provider` and `model` for LLM calls (model can still be
    /// specified here to override the connection's `default_model`).
    #[serde(default)]
    pub provider_connection: Option<String>,
    /// Optional fallback provider connection name. When set, the engine switches to this
    /// provider after `max_consecutive_failures` consecutive LLM errors from the primary.
    /// The switch is per-run only — the next run retries the primary first.
    #[serde(default)]
    pub fallback_provider: Option<String>,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// Maximum output tokens for LLM responses. None = provider default.
    pub max_tokens: Option<u32>,
    pub access: Option<AgentAccessConfig>,
    pub heartbeat: Option<HeartbeatConfig>,
    pub tools: Option<AgentToolPolicy>,
    pub compaction: Option<CompactionConfig>,
    pub session: Option<SessionConfig>,
    /// Maximum number of tools to include in LLM context.
    /// When total tools exceed this limit, the most relevant ones are selected
    /// by keyword matching against the user's message. None = no limit.
    pub max_tools_in_context: Option<usize>,
    /// Multi-provider routing rules. If non-empty, overrides `provider`/`model`.
    /// Evaluated in order — first matching condition is used.
    #[serde(default)]
    pub routing: Vec<ProviderRouteConfig>,
    /// URL path to agent icon image (e.g. "uploads/agent-icon.png").
    /// Served via GET /uploads/{filename}.
    pub icon: Option<String>,
    /// Optional approval config — require owner confirmation before executing specific tools.
    pub approval: Option<ApprovalConfig>,
    /// Tool loop settings — iteration limits, loop detection, overflow recovery.
    pub tool_loop: Option<ToolLoopSettings>,
    /// Watchdog configuration for stuck session detection.
    pub watchdog: Option<WatchdogConfig>,
    /// Base (system) agent: cannot be renamed/deleted via API, SOUL.md and IDENTITY.md
    /// are read-only, runs on host (no Docker sandbox), can write to service source files
    /// and use tools marked `required_base = true`.
    #[serde(default)]
    pub base: bool,
    /// Maximum number of history messages to load into LLM context.
    /// Defaults to 50 when not set.
    #[serde(default)]
    pub max_history_messages: Option<usize>,
    /// Hook configuration — policy enforcement and logging.
    pub hooks: Option<HooksConfig>,
    /// Maximum total tokens (input+output) per day. 0 or absent = unlimited.
    #[serde(default)]
    pub daily_budget_tokens: u64,
    /// Per-agent override for max agent-to-agent turns. None = use global limit.
    #[serde(default)]
    pub max_agent_turns: Option<usize>,
    /// Maximum number of failover attempts per request when multi-provider
    /// routing is configured. Does NOT count the primary call — a value of 3
    /// means "up to 3 fallbacks after primary failed". Cap exists to prevent
    /// unbounded cascading failures across long fallback chains
    /// (re-added after commit c55b039 → 8d33376 regression).
    #[serde(default = "default_max_failover_attempts")]
    pub max_failover_attempts: u32,
}

fn default_max_failover_attempts() -> u32 { 3 }

/// Per-agent hooks configuration (TOML: `[agent.hooks]`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct HooksConfig {
    /// Log every tool call and result via tracing.
    #[serde(default)]
    pub log_all_tool_calls: bool,
    /// Block these tools silently (no approval prompt, just deny).
    #[serde(default)]
    pub block_tools: Vec<String>,
}

/// Per-agent tool loop configuration (TOML: `[agent.tool_loop]`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ToolLoopSettings {
    /// Maximum tool iterations before forcing a final response (default: 50).
    #[serde(default = "default_tool_loop_max")]
    pub max_iterations: usize,
    /// Attempt mid-loop compaction on context overflow (default: true).
    #[serde(default = "default_true")]
    pub compact_on_overflow: bool,
    /// Enable loop detection (default: true).
    #[serde(default = "default_true")]
    pub detect_loops: bool,
    /// Consecutive identical calls before warning (default: 5).
    #[serde(default = "default_tool_loop_warn")]
    pub warn_threshold: usize,
    /// Consecutive identical calls before breaking (default: 10).
    #[serde(default = "default_tool_loop_break")]
    pub break_threshold: usize,
    /// Consecutive LLM errors from primary before switching to fallback provider (default: 3).
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: usize,
    /// Maximum auto-continue nudges per session when LLM response looks incomplete (default: 5).
    #[serde(default = "default_max_auto_continues")]
    pub max_auto_continues: u8,
    /// How many "you're looping" nudges before force-stop (default: 3).
    #[serde(default = "default_max_loop_nudges")]
    pub max_loop_nudges: usize,
    /// Maximum cycle length to detect in n-gram check (3..=N, default: 6).
    #[serde(default = "default_ngram_cycle_length")]
    pub ngram_cycle_length: usize,
    /// Consecutive errors on same tool before breaking (default: 3).
    pub error_break_threshold: Option<usize>,
}

fn default_tool_loop_max() -> usize { 50 }
fn default_tool_loop_warn() -> usize { 5 }
fn default_tool_loop_break() -> usize { 10 }
fn default_max_consecutive_failures() -> usize { 3 }
fn default_max_auto_continues() -> u8 { 5 }
fn default_max_loop_nudges() -> usize { 3 }
fn default_ngram_cycle_length() -> usize { 6 }

/// Approval system configuration for an agent.
/// When enabled, certain tool calls require owner confirmation before execution.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ApprovalConfig {
    /// Master switch: if false, no approvals are required.
    #[serde(default)]
    pub enabled: bool,
    /// Specific tool names that require approval (e.g. ["`shell_exec`", "`code_exec`"]).
    #[serde(default)]
    pub require_for: Vec<String>,
    /// Tool categories that require approval: "system", "destructive", "external".
    #[serde(default)]
    pub require_for_categories: Vec<String>,
    /// Timeout in seconds before auto-rejecting (default: 300 = 5 min).
    #[serde(default = "default_approval_timeout")]
    pub timeout_seconds: u64,
}

fn default_approval_timeout() -> u64 { 300 }

fn default_language() -> String { "ru".to_string() }
fn default_temperature() -> f64 { 1.0 }

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AgentAccessConfig {
    /// Access mode: "open" (anyone can use) or "restricted" (owner + approved users only).
    #[serde(default = "default_access_open")]
    pub mode: String,
    /// User ID of the bot owner (auto-allowed in restricted mode).
    pub owner_id: Option<String>,
}

fn default_access_open() -> String { "open".to_string() }

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HeartbeatConfig {
    pub cron: String,
    pub timezone: Option<String>,
    /// Channel to announce heartbeat results to (e.g. "telegram"). Uses `owner_id` as `chat_id`.
    pub announce_to: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AgentToolPolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub allow_all: bool,
    #[serde(default)]
    pub deny_all_others: bool,
    /// Optional tool group toggles — disable entire groups to save LLM context tokens.
    #[serde(default)]
    pub groups: ToolGroups,
}

/// Toggle switches for internal tool groups.
/// Disabling a group removes those tools from LLM context entirely.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ToolGroups {
    /// `git_status`, `git_diff`, `git_commit`, `git_push`, `git_pull`, `git_ssh_key`
    #[serde(default = "default_true")]
    pub git: bool,
    /// `tool_create`, `tool_list`, `tool_test`, `tool_verify`, `tool_disable`, `tool_discover`
    #[serde(default = "default_true")]
    pub tool_management: bool,
    /// `skill_create`, `skill_update`, `skill_list`
    #[serde(default = "default_true")]
    pub skill_editing: bool,
    /// `sessions_list`, `sessions_history`, `session_search`, `session_context`, `session_send`, `session_export`
    #[serde(default = "default_true")]
    pub session_tools: bool,
}

impl Default for ToolGroups {
    fn default() -> Self {
        Self {
            git: true,
            tool_management: true,
            skill_editing: true,
            session_tools: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CompactionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    #[serde(default)]
    pub preserve_tool_calls: bool,
    #[serde(default = "default_preserve_last_n")]
    pub preserve_last_n: u32,
    /// Override max context tokens (default: auto-detect from model name).
    pub max_context_tokens: Option<u32>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.8,
            preserve_tool_calls: false,
            preserve_last_n: 10,
            max_context_tokens: None,
        }
    }
}

fn default_threshold() -> f64 { 0.8 }
fn default_preserve_last_n() -> u32 { 10 }

/// Session management config (per-agent).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SessionConfig {
    /// DM scope: "shared" (all DMs one session), "per-channel-peer" (default),
    /// "per-peer" (same user across channels shares session), "per-chat" (by `chat_id`).
    #[serde(default = "default_dm_scope")]
    pub dm_scope: String,
    /// Delete sessions older than this many days (0 = never).
    #[serde(default = "default_session_ttl_days")]
    pub ttl_days: u32,
    /// Maximum messages per session (0 = unlimited). Oldest are trimmed.
    #[serde(default)]
    pub max_messages: u32,
    /// Proactively strip tool result content older than this many user turns at context load time.
    /// Complements `compact_tool_results` (reactive, token-based) — this fires before the first LLM call.
    /// Tool results are replaced with "[output omitted, N chars]".
    /// None = no proactive pruning (default).
    #[serde(default)]
    pub prune_tool_output_after_turns: Option<usize>,
}

fn default_dm_scope() -> String { "per-channel-peer".to_string() }
fn default_session_ttl_days() -> u32 { 30 }

/// Watchdog configuration for stuck session detection.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WatchdogConfig {
    /// Seconds of inactivity before watchdog kills the session.
    /// Inactivity = no `upsert_streaming_message` or tool result written.
    /// Default: 600 (10 minutes).
    #[serde(default = "default_watchdog_inactivity_secs")]
    pub inactivity_secs: u64,
}

fn default_watchdog_inactivity_secs() -> u64 { 600 }

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            dm_scope: default_dm_scope(),
            ttl_days: default_session_ttl_days(),
            max_messages: 0,
            prune_tool_output_after_turns: None,
        }
    }
}

// ── Sandbox config ──

/// Configuration for the code execution sandbox (`code_exec` tool).
/// Requires Docker to be available.
///
/// Configured under `[sandbox]` in hydeclaw.toml:
/// ```toml
/// [sandbox]
/// enabled = true
/// image = "python:3.12-slim"
/// timeout_secs = 30
/// memory_mb = 256
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SandboxConfig {
    /// Enable the `code_exec` tool. Requires Docker.
    #[serde(default)]
    pub enabled: bool,
    /// Docker image used for code execution.
    #[serde(default = "default_sandbox_image")]
    pub image: String,
    /// Execution timeout in seconds before the container is killed.
    #[serde(default = "default_sandbox_timeout")]
    pub timeout_secs: u64,
    /// Memory limit per execution in megabytes.
    #[serde(default = "default_sandbox_memory")]
    pub memory_mb: u32,
    /// Extra volume mounts for agent containers (e.g. "docker/toolgate:/toolgate").
    /// Relative paths are resolved against the project root (workspace parent).
    #[serde(default)]
    pub extra_binds: Vec<String>,
}

fn default_sandbox_image() -> String { "python:3.12-slim".to_string() }
fn default_sandbox_timeout() -> u64 { 30 }
fn default_sandbox_memory() -> u32 { 256 }

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            image: default_sandbox_image(),
            timeout_secs: default_sandbox_timeout(),
            memory_mb: default_sandbox_memory(),
            extra_binds: vec![],
        }
    }
}

// ── Docker service management config ──

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DockerConfig {
    /// Path to docker-compose.yml (relative to working directory or absolute).
    #[serde(default = "default_compose_file")]
    pub compose_file: String,
    /// Whitelist of service names allowed for rebuild/restart via API.
    #[serde(default)]
    pub rebuild_allowed: Vec<String>,
    /// Timeout for rebuild command in seconds.
    #[serde(default = "default_rebuild_timeout")]
    pub rebuild_timeout_secs: u64,
}

fn default_compose_file() -> String { "docker/docker-compose.yml".into() }
fn default_rebuild_timeout() -> u64 { 300 }

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            compose_file: default_compose_file(),
            rebuild_allowed: vec![],
            rebuild_timeout_secs: default_rebuild_timeout(),
        }
    }
}

// ── Tailscale Funnel config ──

/// Expose the gateway via `tailscale serve` / `tailscale funnel`.
/// Requires the `tailscale` binary on the host.
///
/// ```toml
/// [tailscale]
/// enabled = true
/// funnel = true   # public internet access (false = Tailnet only)
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
pub struct TailscaleConfig {
    /// Enable Tailscale serve integration.
    #[serde(default)]
    pub enabled: bool,
    /// If true, use `tailscale funnel` (public). If false, `tailscale serve` (Tailnet only).
    #[serde(default)]
    pub funnel: bool,
}


impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read config: {}", path.as_ref().display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| "failed to parse config TOML")?;
        Ok(config)
    }
}

impl AgentConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read agent config: {}", path.as_ref().display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| "failed to parse agent config TOML")?;
        Ok(config)
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).with_context(|| "failed to serialize agent config to TOML")
    }
}

/// Load all agent configs from a directory of TOML files.
pub fn load_agent_configs(dir: &str) -> Result<Vec<AgentConfig>> {
    let mut configs = vec![];
    let path = Path::new(dir);
    if !path.exists() {
        return Ok(configs);
    }

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_path = entry.path();
        if file_path.extension().is_some_and(|e| e == "toml") {
            match AgentConfig::load(&file_path) {
                Ok(cfg) => {
                    tracing::info!(agent = %cfg.agent.name, file = %file_path.display(), "loaded agent config");
                    configs.push(cfg);
                }
                Err(e) => {
                    tracing::warn!(file = %file_path.display(), error = %e, "failed to load agent config");
                }
            }
        }
    }

    Ok(configs)
}

// ── Config file update (preserves comments) ──

/// Update service URLs in the TOML config file.
/// Uses `toml_edit` to preserve comments and formatting.
pub fn update_service_urls(
    config_path: &str,
    toolgate_url: Option<&str>,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content.parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    // toolgate_url: top-level key + [tools.toolgate].url
    if let Some(url) = toolgate_url {
        if url.is_empty() {
            doc.remove("toolgate_url");
        } else {
            doc["toolgate_url"] = toml_edit::value(url);
        }
        // Also update [tools.toolgate].url (used by ToolRegistry health checks)
        if let Some(tools) = doc.get_mut("tools")
            && let Some(mp) = tools.get_mut("toolgate") {
                mp["url"] = toml_edit::value(if url.is_empty() { "" } else { url });
            }
    }

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;

    Ok(())
}

/// Update [memory] section in TOML config file.
pub fn update_memory_config(
    config_path: &str,
    enabled: Option<bool>,
    embed_url: Option<&str>,
    embed_model: Option<&str>,
    embed_dim: Option<u32>,
    embed_dimensions: Option<u32>,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content.parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    // Ensure [memory] section exists
    if doc.get("memory").is_none() {
        doc["memory"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    if let Some(en) = enabled {
        doc["memory"]["enabled"] = toml_edit::value(en);
    }

    if let Some(url) = embed_url {
        if url.is_empty() {
            if let Some(mem) = doc.get_mut("memory") {
                mem.as_table_mut().map(|t| t.remove("embed_url"));
            }
        } else {
            doc["memory"]["embed_url"] = toml_edit::value(url);
        }
    }

    if let Some(model) = embed_model {
        if model.is_empty() {
            if let Some(mem) = doc.get_mut("memory") {
                mem.as_table_mut().map(|t| t.remove("embed_model"));
            }
        } else {
            doc["memory"]["embed_model"] = toml_edit::value(model);
        }
    }

    if let Some(dim) = embed_dim {
        if dim == 0 {
            if let Some(mem) = doc.get_mut("memory") {
                mem.as_table_mut().map(|t| t.remove("embed_dim"));
            }
        } else {
            doc["memory"]["embed_dim"] = toml_edit::value(i64::from(dim));
        }
    }

    if let Some(dim) = embed_dimensions {
        if dim == 0 {
            if let Some(mem) = doc.get_mut("memory") {
                mem.as_table_mut().map(|t| t.remove("embed_dimensions"));
            }
        } else {
            doc["memory"]["embed_dimensions"] = toml_edit::value(i64::from(dim));
        }
    }

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;

    Ok(())
}

pub fn update_subagents_enabled(
    config_path: &str,
    enabled: bool,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content.parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    if doc.get("subagents").is_none() {
        doc["subagents"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    doc["subagents"]["enabled"] = toml_edit::value(enabled);

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;

    Ok(())
}

/// Update [limits] section in TOML config file.
pub fn update_limits_config(
    config_path: &str,
    max_requests_per_minute: Option<u32>,
    max_tool_concurrency: Option<u32>,
    max_agent_turns: Option<usize>,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content.parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    if doc.get("limits").is_none() {
        doc["limits"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    if let Some(v) = max_requests_per_minute {
        doc["limits"]["max_requests_per_minute"] = toml_edit::value(i64::from(v));
    }

    if let Some(v) = max_tool_concurrency {
        doc["limits"]["max_tool_concurrency"] = toml_edit::value(i64::from(v));
    }

    if let Some(v) = max_agent_turns {
        doc["limits"]["max_agent_turns"] = toml_edit::value(v as i64);
    }

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;

    Ok(())
}

/// Update [gateway].`public_url` in TOML config file.
pub fn update_public_url(
    config_path: &str,
    public_url: &str,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content.parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    if doc.get("gateway").is_none() {
        doc["gateway"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    if public_url.is_empty() {
        if let Some(gw) = doc.get_mut("gateway") {
            gw.as_table_mut().map(|t| t.remove("public_url"));
        }
    } else {
        doc["gateway"]["public_url"] = toml_edit::value(public_url);
    }

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;

    Ok(())
}

/// Update [backup] section in TOML config file.
pub fn update_backup_config(
    config_path: &str,
    enabled: Option<bool>,
    cron: Option<&str>,
    retention_days: Option<u32>,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content.parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    if doc.get("backup").is_none() {
        doc["backup"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    if let Some(v) = enabled {
        doc["backup"]["enabled"] = toml_edit::value(v);
    }

    if let Some(v) = cron {
        doc["backup"]["cron"] = toml_edit::value(v);
    }

    if let Some(v) = retention_days {
        doc["backup"]["retention_days"] = toml_edit::value(i64::from(v));
    }

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;

    Ok(())
}

// ── Config hot-reload ──

use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared config handle that supports atomic hot-reload.
pub type SharedConfig = Arc<RwLock<AppConfig>>;

/// Atomic flag to suppress file-watcher reload when the API just wrote the file.
/// Set to `true` by the API handler before writing; watcher skips one cycle.
pub type ConfigApiWriteFlag = Arc<std::sync::atomic::AtomicBool>;

/// Watch config file for changes and reload atomically.
/// Debounces changes (500ms) and validates before applying.
/// Skips reload when `api_write_flag` is set (API handler already updated in-memory config).
pub fn spawn_config_watcher(config_path: String, shared: SharedConfig, api_write_flag: ConfigApiWriteFlag) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    // Capture tokio runtime handle before spawning OS thread
    let rt = tokio::runtime::Handle::current();

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "failed to create config file watcher");
                return;
            }
        };

        if let Err(e) = watcher.watch(Path::new(&config_path), RecursiveMode::NonRecursive) {
            tracing::error!(error = %e, path = %config_path, "failed to watch config file");
            return;
        }

        tracing::info!(path = %config_path, "config file watcher started");
        let mut last_reload = std::time::Instant::now();

        for event in rx {
            match event {
                Ok(Event {
                    kind: EventKind::Modify(_),
                    ..
                }) => {
                    // Debounce: skip if less than 500ms since last reload.
                    // Consume the API-write flag even on debounce so it doesn't leak to the next event.
                    if last_reload.elapsed() < std::time::Duration::from_millis(500) {
                        api_write_flag.swap(false, std::sync::atomic::Ordering::AcqRel);
                        continue;
                    }

                    // Skip if the API handler just wrote this file (it already updated in-memory config)
                    if api_write_flag.swap(false, std::sync::atomic::Ordering::AcqRel) {
                        tracing::debug!("config watcher: skipping reload (API-initiated write)");
                        last_reload = std::time::Instant::now();
                        continue;
                    }

                    // Small delay to let the editor finish writing
                    std::thread::sleep(std::time::Duration::from_millis(200));

                    match AppConfig::load(&config_path) {
                        Ok(new_config) => {
                            let shared = shared.clone();
                            rt.spawn(async move {
                                let mut config = shared.write().await;
                                *config = new_config;
                                tracing::info!("config reloaded successfully");
                            });
                            last_reload = std::time::Instant::now();
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "invalid config, keeping current");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "config watcher error");
                }
                _ => {}
            }
        }
    });
}

/// Global LLM parameter defaults applied when agent config doesn't specify them.
/// Priority: agent config → [agent.defaults] → provider defaults.
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
pub struct AgentDefaultsConfig {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
}

/// Wrapper for the [agent] section in hydeclaw.toml (global defaults).
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
pub struct AgentSectionConfig {
    #[serde(default)]
    pub defaults: AgentDefaultsConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. AgentConfig TOML parse from minimal string ──

    #[test]
    fn agent_config_parse_minimal_toml() {
        let toml_str = r#"
[agent]
name = "test"
provider = "minimax"
model = "m2.5"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("failed to parse minimal AgentConfig");
        assert_eq!(cfg.agent.name, "test");
        assert_eq!(cfg.agent.provider, "minimax");
        assert_eq!(cfg.agent.model, "m2.5");
        // Defaults kick in
        assert_eq!(cfg.agent.language, "ru");
        assert_eq!(cfg.agent.temperature, 1.0);
        assert!(cfg.agent.access.is_none());
        assert!(cfg.agent.heartbeat.is_none());
        assert!(cfg.agent.tools.is_none());
        assert!(cfg.agent.compaction.is_none());
        assert!(cfg.agent.session.is_none());
        assert!(cfg.agent.max_tools_in_context.is_none());
        assert!(cfg.agent.routing.is_empty());
        assert!(cfg.agent.icon.is_none());
        assert!(cfg.agent.approval.is_none());
        assert!(cfg.agent.tool_loop.is_none());
    }

    // ── 2. AgentConfig roundtrip: serialize → deserialize → compare ──

    #[test]
    fn agent_config_roundtrip() {
        let original = AgentConfig {
            agent: AgentSettings {
                name: "roundtrip-agent".into(),
                language: "en".into(),
                provider: "openai".into(),
                model: "gpt-4".into(),
                temperature: 0.7,
                max_tokens: None,
                access: Some(AgentAccessConfig {
                    mode: "restricted".into(),
                    owner_id: Some("12345".into()),
                }),
                heartbeat: None,
                tools: None,
                compaction: Some(CompactionConfig {
                    enabled: true,
                    threshold: 0.9,
                    preserve_tool_calls: true,
                    preserve_last_n: 5,
                    max_context_tokens: Some(8000),
                }),
                session: Some(SessionConfig {
                    dm_scope: "shared".into(),
                    ttl_days: 7,
                    max_messages: 100,
                    prune_tool_output_after_turns: None,
                }),
                max_tools_in_context: Some(20),
                max_history_messages: None,
                routing: vec![ProviderRouteConfig {
                    condition: "default".into(),
                    connection: Some("minimax-default".into()),
                    model: Some("m2.5".into()),
                    temperature: Some(0.8),
                    cooldown_secs: 60,
                }],
                icon: None,
                approval: None,
                tool_loop: None,
                base: false,
                watchdog: None,
                provider_connection: None,
                fallback_provider: None,
                hooks: None,
                daily_budget_tokens: 0,
                max_agent_turns: None,
                max_failover_attempts: default_max_failover_attempts(),
            },
        };

        let toml_str = original.to_toml().expect("serialize failed");
        let restored: AgentConfig =
            toml::from_str(&toml_str).expect("deserialize roundtrip failed");
        assert_eq!(original, restored);
    }

    // ── 3. AgentConfig roundtrip with approval and tool_loop ──

    #[test]
    fn agent_config_roundtrip_with_approval() {
        let original = AgentConfig {
            agent: AgentSettings {
                name: "full-agent".into(),
                language: "ru".into(),
                provider: "minimax".into(),
                model: "m2.5".into(),
                temperature: 1.0,
                max_tokens: None,
                access: None,
                heartbeat: Some(HeartbeatConfig {
                    cron: "0 */30 10-19 * * *".into(),
                    timezone: Some("Europe/Samara".into()),
                    announce_to: Some("telegram".into()),
                }),
                tools: Some(AgentToolPolicy {
                    allow: vec!["memory".into()],
                    deny: vec!["shell_exec".into()],
                    allow_all: false,
                    deny_all_others: false,
                    groups: ToolGroups {
                        git: false,
                        tool_management: true,
                        skill_editing: true,
                        session_tools: false,
                    },
                }),
                compaction: None,
                session: None,
                max_tools_in_context: None,
                max_history_messages: None,
                routing: vec![],
                icon: Some("uploads/icon.png".into()),
                approval: Some(ApprovalConfig {
                    enabled: true,
                    require_for: vec!["shell_exec".into()],
                    require_for_categories: vec!["destructive".into()],
                    timeout_seconds: 120,
                }),
                tool_loop: Some(ToolLoopSettings {
                    max_iterations: 30,
                    compact_on_overflow: true,
                    detect_loops: true,
                    warn_threshold: 3,
                    break_threshold: 7,
                    max_consecutive_failures: 3,
                    max_auto_continues: 5,
                    max_loop_nudges: 3,
                    ngram_cycle_length: 6,
                    error_break_threshold: None,
                }),
                base: false,
                watchdog: None,
                provider_connection: None,
                fallback_provider: None,
                hooks: None,
                daily_budget_tokens: 0,
                max_agent_turns: None,
                max_failover_attempts: default_max_failover_attempts(),
            },
        };

        let toml_str = original.to_toml().expect("serialize failed");
        let restored: AgentConfig =
            toml::from_str(&toml_str).expect("deserialize roundtrip failed");
        assert_eq!(original, restored);
    }

    // ── 4. LimitsConfig defaults ──

    #[test]
    fn limits_config_defaults() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_requests_per_minute, 300);
        assert_eq!(cfg.max_tool_concurrency, 10);
        assert_eq!(cfg.request_timeout_secs, 180);
        assert_eq!(cfg.max_agent_turns, 5);
        // Phase 64 SEC-04: new [limits] key — default 500 MB.
        assert_eq!(cfg.max_restore_size_mb, 500);
    }

    // ── 4d. UploadsConfig defaults (Phase 64 SEC-03) ──

    #[test]
    fn uploads_config_defaults() {
        let cfg = UploadsConfig::default();
        assert_eq!(cfg.signed_url_ttl_secs, 86_400);
        assert!(!cfg.require_signature, "v0.19.0 grace period keeps this false");
    }

    #[test]
    fn uploads_config_parses_custom_values() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[uploads]
signed_url_ttl_secs = 3600
require_signature = true
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.uploads.signed_url_ttl_secs, 3600);
        assert!(cfg.uploads.require_signature);
    }

    #[test]
    fn uploads_config_missing_section_uses_defaults() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.uploads.signed_url_ttl_secs, 86_400);
        assert!(!cfg.uploads.require_signature);
    }

    // ── 4a. AgentSettings max_agent_turns defaults to None ──

    #[test]
    fn agent_config_max_agent_turns_none_by_default() {
        let toml_str = r#"
[agent]
name = "test"
provider = "minimax"
model = "m2.5"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
        assert!(cfg.agent.max_agent_turns.is_none());
    }

    // ── 4b. AgentSettings max_agent_turns override ──

    #[test]
    fn agent_config_max_agent_turns_override() {
        let toml_str = r#"
[agent]
name = "test"
provider = "minimax"
model = "m2.5"
max_agent_turns = 3
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.agent.max_agent_turns, Some(3));
    }

    // ── 4c. LimitsConfig max_agent_turns custom value ──

    #[test]
    fn limits_config_max_agent_turns_custom() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[limits]
max_agent_turns = 10
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.limits.max_agent_turns, 10);
    }

    // ── 4e. LimitsConfig max_restore_size_mb (Phase 64 SEC-04) ──

    #[test]
    fn limits_config_max_restore_size_mb_custom() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[limits]
max_restore_size_mb = 250
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.limits.max_restore_size_mb, 250);
    }

    #[test]
    fn limits_config_max_restore_size_mb_missing_uses_default() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[limits]
max_requests_per_minute = 200
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.limits.max_restore_size_mb, 500, "missing key uses default");
    }

    // ── 5. SubagentsConfig defaults ──

    #[test]
    fn subagents_config_defaults() {
        let cfg = SubagentsConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.default_mode, "in-process");
        assert_eq!(cfg.max_concurrent_in_process, 5);
        assert_eq!(cfg.max_concurrent_docker, 3);
        assert!(cfg.core_image.is_none());
    }

    // ── 7. CompactionConfig defaults ──

    #[test]
    fn compaction_config_defaults() {
        let cfg = CompactionConfig::default();
        assert!(cfg.enabled);
        assert!((cfg.threshold - 0.8).abs() < f64::EPSILON);
        assert!(!cfg.preserve_tool_calls);
        assert_eq!(cfg.preserve_last_n, 10);
        assert!(cfg.max_context_tokens.is_none());
    }

    // ── 8. SessionConfig defaults ──

    #[test]
    fn session_config_defaults() {
        let cfg = SessionConfig::default();
        assert_eq!(cfg.dm_scope, "per-channel-peer");
        assert_eq!(cfg.ttl_days, 30);
        assert_eq!(cfg.max_messages, 0);
    }

    // ── 9. SandboxConfig defaults ──

    #[test]
    fn sandbox_config_defaults() {
        let cfg = SandboxConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.image, "python:3.12-slim");
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.memory_mb, 256);
        assert!(cfg.extra_binds.is_empty());
    }

    // ── 10. DockerConfig defaults ──

    #[test]
    fn docker_config_defaults() {
        let cfg = DockerConfig::default();
        assert_eq!(cfg.compose_file, "docker/docker-compose.yml");
        assert_eq!(cfg.rebuild_timeout_secs, 300);
        assert!(cfg.rebuild_allowed.is_empty());
    }

    // ── 11. TailscaleConfig defaults ──

    #[test]
    fn tailscale_config_defaults() {
        let cfg = TailscaleConfig::default();
        assert!(!cfg.enabled);
        assert!(!cfg.funnel);
    }

    // ── 12. ToolGroups defaults (all true) ──

    #[test]
    fn tool_groups_defaults() {
        let cfg = ToolGroups::default();
        assert!(cfg.git);
        assert!(cfg.tool_management);
        assert!(cfg.skill_editing);
        assert!(cfg.session_tools);
    }

    // ── 13. AppConfig minimal TOML deserialization ──

    #[test]
    fn app_config_parse_minimal_toml() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("failed to parse minimal AppConfig");
        assert_eq!(cfg.gateway.listen, "0.0.0.0:18789");
        assert!(cfg.gateway.auth_token_env.is_none());
        assert!(cfg.gateway.public_url.is_none());
        assert_eq!(cfg.database.url, "postgres://localhost/test");
        // All optional sections should use defaults
        assert_eq!(cfg.limits.max_requests_per_minute, 300);
        assert_eq!(cfg.limits.max_tool_concurrency, 10);
        assert!(cfg.subagents.enabled);
        assert_eq!(cfg.subagents.default_mode, "in-process");
        assert!(cfg.mcp.is_empty());
        assert!(!cfg.sandbox.enabled);
        assert_eq!(cfg.docker.compose_file, "docker/docker-compose.yml");
        assert!(cfg.toolgate_url.is_none());
        assert!(!cfg.tailscale.enabled);
        assert!(!cfg.tailscale.funnel);
    }

    // ── 14. AppConfig with overridden sections ──

    #[test]
    fn app_config_parse_with_overrides() {
        let toml_str = r#"
toolgate_url = "http://localhost:8888"

[gateway]
listen = "127.0.0.1:9999"
auth_token_env = "MY_TOKEN"

[database]
url = "postgres://user:pass@db:5432/hydeclaw"

[limits]
max_requests_per_minute = 200
max_tool_concurrency = 20

[sandbox]
enabled = true
image = "node:20-slim"
timeout_secs = 60
memory_mb = 512

[tailscale]
enabled = true
funnel = true
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("failed to parse AppConfig with overrides");
        assert_eq!(cfg.gateway.listen, "127.0.0.1:9999");
        assert_eq!(cfg.gateway.auth_token_env.as_deref(), Some("MY_TOKEN"));
        assert_eq!(cfg.database.url, "postgres://user:pass@db:5432/hydeclaw");
        assert_eq!(cfg.limits.max_requests_per_minute, 200);
        assert_eq!(cfg.limits.max_tool_concurrency, 20);
        assert!(cfg.sandbox.enabled);
        assert_eq!(cfg.sandbox.image, "node:20-slim");
        assert_eq!(cfg.sandbox.timeout_secs, 60);
        assert_eq!(cfg.sandbox.memory_mb, 512);
        assert!(cfg.tailscale.enabled);
        assert!(cfg.tailscale.funnel);
        assert_eq!(cfg.toolgate_url.as_deref(), Some("http://localhost:8888"));
    }

    // ── 15. McpFileEntry::to_config() field mapping ──

    #[test]
    fn mcp_file_entry_to_config() {
        let entry = McpFileEntry {
            name: "test-mcp".into(),
            url: Some("http://localhost:3000/mcp".into()),
            container: Some("mcp-test".into()),
            port: Some(3000),
            mode: "always-on".into(),
            idle_timeout: Some("10m".into()),
            protocol: "sse".into(),
            enabled: true,
        };

        let config = entry.to_config();

        // name should NOT be in McpConfig — it's only on McpFileEntry
        assert_eq!(config.url, entry.url);
        assert_eq!(config.container, entry.container);
        assert_eq!(config.port, entry.port);
        assert_eq!(config.mode, entry.mode);
        assert_eq!(config.idle_timeout, entry.idle_timeout);
        assert_eq!(config.protocol, entry.protocol);
        assert_eq!(config.enabled, entry.enabled);
    }

    // ── 16. McpFileEntry::to_config() with defaults ──

    #[test]
    fn mcp_file_entry_to_config_defaults() {
        let yaml_str = "name: minimal-mcp";
        let entry: McpFileEntry = serde_yaml::from_str(yaml_str)
            .expect("failed to parse minimal McpFileEntry");

        assert_eq!(entry.name, "minimal-mcp");
        assert!(entry.url.is_none());
        assert!(entry.container.is_none());
        assert!(entry.port.is_none());
        assert_eq!(entry.mode, "on-demand");
        assert!(entry.idle_timeout.is_none());
        assert_eq!(entry.protocol, "mcp");
        assert!(entry.enabled);

        let config = entry.to_config();
        assert!(config.url.is_none());
        assert_eq!(config.mode, "on-demand");
        assert_eq!(config.protocol, "mcp");
        assert!(config.enabled);
    }

    // ── 17. McpFileEntry::config_ref() returns same as to_config() ──


    // ── 18. ToolLoopSettings defaults via TOML deserialization ──

    #[test]
    fn tool_loop_settings_defaults_via_toml() {
        let toml_str = r#"
[agent]
name = "loop-test"
provider = "minimax"
model = "m1"

[agent.tool_loop]
"#;
        let cfg: AgentConfig =
            toml::from_str(toml_str).expect("failed to parse AgentConfig with tool_loop");
        let tl = cfg.agent.tool_loop.expect("tool_loop should be Some");
        assert_eq!(tl.max_iterations, 50);
        assert!(tl.compact_on_overflow);
        assert!(tl.detect_loops);
        assert_eq!(tl.warn_threshold, 5);
        assert_eq!(tl.break_threshold, 10);
    }

    // ── 19. GatewayConfig default listen address ──

    #[test]
    fn gateway_config_default_listen() {
        let toml_str = r#"
[gateway]

[database]
url = "postgres://localhost/test"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("failed to parse");
        assert_eq!(cfg.gateway.listen, "0.0.0.0:18789");
    }

    // ── 21. ApprovalConfig default timeout ──

    #[test]
    fn approval_config_default_timeout() {
        let toml_str = r#"
[agent]
name = "approval-test"
provider = "minimax"
model = "m2.5"

[agent.approval]
enabled = true
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("failed to parse");
        let approval = cfg.agent.approval.expect("approval should be Some");
        assert!(approval.enabled);
        assert_eq!(approval.timeout_seconds, 300);
        assert!(approval.require_for.is_empty());
        assert!(approval.require_for_categories.is_empty());
    }

    // ── 22. ProviderRouteConfig default condition ──

    #[test]
    fn provider_route_default_condition() {
        let toml_str = r#"
[agent]
name = "route-test"
provider = "minimax"
model = "m2.5"

[[agent.routing]]
connection = "openai-default"
model = "gpt-4"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("failed to parse");
        assert_eq!(cfg.agent.routing.len(), 1);
        assert_eq!(cfg.agent.routing[0].condition, "default");
        assert_eq!(cfg.agent.routing[0].connection.as_deref(), Some("openai-default"));
        assert_eq!(cfg.agent.routing[0].model.as_deref(), Some("gpt-4"));
        assert!(cfg.agent.routing[0].temperature.is_none());
    }

    // ── Task 11: new connection-based route tests ──

    #[test]
    fn route_parses_connection_reference_only() {
        let toml_src = r#"
            connection = "ollama-default"
            model = "minimax-m2.7"
            condition = "default"
            cooldown_secs = 60
        "#;
        let route: ProviderRouteConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(route.connection.as_deref(), Some("ollama-default"));
        assert_eq!(route.model.as_deref(), Some("minimax-m2.7"));
        assert_eq!(route.cooldown_secs, 60);
    }

    #[test]
    fn route_rejects_cooldown_zero_at_validate() {
        let route = ProviderRouteConfig {
            condition: "default".into(),
            connection: Some("p".into()),
            model: None,
            temperature: None,
            cooldown_secs: 0,
        };
        assert!(route.validate().is_err());
    }

    // ── 23. ToolGroups partial override via TOML ──

    #[test]
    fn tool_groups_partial_override() {
        let toml_str = r#"
[agent]
name = "groups-test"
provider = "minimax"
model = "m2.5"

[agent.tools]
allow_all = true

[agent.tools.groups]
git = false
session_tools = false
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("failed to parse");
        let tools = cfg.agent.tools.expect("tools should be Some");
        assert!(!tools.groups.git);
        assert!(tools.groups.tool_management); // default true
        assert!(tools.groups.skill_editing);   // default true
        assert!(!tools.groups.session_tools);
    }

    // ── LimitsConfig: max_inter_agent_context_chars (inter-agent context) ──

    #[test]
    fn limits_config_default_inter_agent_context_chars() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_inter_agent_context_chars, 2000);
    }

    #[test]
    fn limits_config_custom_inter_agent_context_chars() {
        let toml_str = r#"
max_inter_agent_context_chars = 500
"#;
        let cfg: LimitsConfig = toml::from_str(toml_str).expect("failed to parse");
        assert_eq!(cfg.max_inter_agent_context_chars, 500);
        // Other fields should get defaults
        assert_eq!(cfg.max_requests_per_minute, 300);
        assert_eq!(cfg.max_agent_turns, 5);
    }

}

#[cfg(test)]
mod defaults_tests {
    use super::*;

    #[test]
    fn agent_defaults_deserialize_from_toml() {
        let toml_str = r#"
[agent.defaults]
temperature = 0.5
max_tokens = 2048
"#;
        #[derive(serde::Deserialize)]
        struct Wrapper {
            agent: AgentSectionConfig,
        }
        let w: Wrapper = toml::from_str(toml_str).unwrap();
        assert_eq!(w.agent.defaults.temperature, Some(0.5));
        assert_eq!(w.agent.defaults.max_tokens, Some(2048));
    }

    #[test]
    fn agent_defaults_missing_is_none() {
        let cfg = AgentDefaultsConfig::default();
        assert!(cfg.temperature.is_none());
        assert!(cfg.max_tokens.is_none());
    }
}
