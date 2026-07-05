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
    #[schemars(skip)]
    pub mcp: HashMap<String, McpConfig>,
    #[serde(default)]
    pub memory: crate::memory::MemoryConfig,
    #[serde(default)]
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
    /// External model-metadata catalog (context windows). See
    /// docs/architecture/2026-07-05-model-catalog-multicatalog.md
    #[serde(default)]
    pub model_catalog: ModelCatalogConfig,
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
    /// Scheduled skill curator (disabled by default).
    #[serde(default)]
    pub curator: CuratorConfig,
    /// Phase 62 RES-03 cleanup scheduler tuning (session_timeline retention).
    #[serde(default)]
    pub cleanup: CleanupConfig,
    /// Phase 62 RES-05 graceful-shutdown drain tuning (drain timeout).
    #[serde(default)]
    pub shutdown: ShutdownConfig,
    /// Phase 64 SEC-03 upload URL signing config.
    #[serde(default)]
    pub uploads: UploadsConfig,
    /// Multi-agent (`agent` tool) timeouts. Hot-reloadable via the config
    /// watcher and the `/api/config` PUT endpoint.
    #[serde(default)]
    pub agent_tool: AgentToolConfig,
    /// YAML-tool response cache tuning. Process-wide singleton shared
    /// across all agents via `Arc<ToolExecutionContext>`.
    #[serde(default)]
    pub tools_cache: ToolCacheConfig,
    /// Per-tool overrides for the semantic SEARCH cache (`[semantic_cache]`).
    #[serde(default)]
    pub semantic_cache: SemanticCacheConfig,
    /// Agent web-fetch guardrails (domain blocklist).
    #[serde(default)]
    pub security: SecurityConfig,
    /// Shadow-git checkpoint snapshots (enabled by default).
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
    /// Language server (LSP) orchestration config (disabled by default).
    #[serde(default)]
    pub lsp: LspConfig,
    /// Video-summarisation tunables (`[video]` section).
    #[serde(default)]
    pub video: VideoConfig,
}

// ── SemanticCacheConfig ───────────────────────────────────────────────────────

// NOTE: AppConfig derives JsonSchema, so every nested config type MUST derive it too,
// and a `#[serde(flatten)]` map needs `#[schemars(skip)]` (mirrors `AppConfig.mcp`).

/// Per-tool override for the semantic SEARCH cache (distinct from the YAML-tool
/// response cache `ToolCacheConfig`/`tools_cache`). TOML: `[semantic_cache]`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
pub struct SemanticCacheToolConfig {
    pub ttl_secs: u64,
    pub threshold: f32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct SemanticCacheConfig {
    /// tool_name → override. Missing built-in tools fall back to the 3600/0.95 default.
    #[serde(flatten)]
    #[schemars(skip)]
    pub tools: HashMap<String, SemanticCacheToolConfig>,
}

impl SemanticCacheConfig {
    /// The four built-in cacheable search tools; used when a tool has no explicit override.
    fn builtin_default(tool: &str) -> Option<SemanticCacheToolConfig> {
        matches!(tool, "searxng_search" | "brave_search" | "browser_render" | "web_search")
            .then_some(SemanticCacheToolConfig { ttl_secs: 3600, threshold: 0.95 })
    }
    /// Resolve a tool's cache config: explicit override wins, else built-in default, else None.
    pub fn for_tool(&self, tool: &str) -> Option<SemanticCacheToolConfig> {
        self.tools.get(tool).copied().or_else(|| Self::builtin_default(tool))
    }
}

// ── SecurityConfig ────────────────────────────────────────────────────────────

/// Operator-configurable web-fetch guardrails for agents.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct SecurityConfig {
    /// Glob domains the agent may not fetch (e.g. "*.evil.tld"). Empty = no policy.
    #[serde(default)]
    pub blocked_domains: Vec<String>,
}

// ── UploadsConfig (Phase 64 SEC-03) ───────────────────────────────────────────

/// Signed-URL configuration for uploads and workspace-files.
///
/// Enforced since v0.26.0 (default `true`). Set to `false` in your
/// `opex.toml` only if you need to serve unsigned legacy URLs.
///
/// **Scope:** `signed_url_ttl_secs` governs (a) `POST /api/media/upload`
/// (client_upload rows) and (b) workspace-files URLs. It does NOT govern
/// agent_icon rows (URL TTL = `HISTORICAL_URL_TTL_SECS`, ~50 years, since
/// the row never expires) nor tool_output rows (URL TTL =
/// `cleanup.uploads_retention_days * 86_400`, matched to row lifetime so
/// URL and row expire together).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UploadsConfig {
    /// TTL for signed `/api/media/upload` and workspace-files URLs (seconds).
    /// Default: 24 h (86_400 s). See struct-level doc for scope.
    #[serde(default = "default_signed_url_ttl")]
    pub signed_url_ttl_secs: u64,
    /// Enforce HMAC verification on every signed-URL request.
    /// Default: `true` (enforced). Set `false` to accept unsigned URLs.
    #[serde(default = "default_require_signature")]
    pub require_signature: bool,
    /// Per-file upload byte ceiling. Single source of truth for the request-layer
    /// caps (DefaultBodyLimit + the `POST /api/media/upload` guard) — the DB
    /// CHECK (migration 062) and `db::uploads::MAX_UPLOAD_BYTES` are the matched
    /// backstops. Default: 50 MB. Toolgate VISION_MAX_BYTES (20 MB) is independent.
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
}

fn default_signed_url_ttl() -> u64 {
    86_400
}
fn default_require_signature() -> bool {
    true
}
fn default_max_upload_bytes() -> u64 {
    52_428_800
}

impl Default for UploadsConfig {
    fn default() -> Self {
        Self {
            signed_url_ttl_secs: default_signed_url_ttl(),
            require_signature: default_require_signature(),
            max_upload_bytes: default_max_upload_bytes(),
        }
    }
}

// ── AgentToolConfig (multi-agent timeouts) ────────────────────────────────────

/// Timeouts for the multi-agent `agent` tool (run/message/collect/status/kill).
///
/// All values are in seconds. The defaults match the previous compile-time
/// constants:
/// - `message_wait_for_idle_secs` = 60 (how long to wait for the target agent
///   to become idle before sending it a new message).
/// - `message_result_secs` = 300 (how long sync `run`, sync `message`, and
///   `collect` block waiting for the target's `last_result`).
/// - `safety_timeout_secs` = 600 (defense-in-depth outer wrapper around any
///   `agent` tool call in `pipeline::parallel`).
///
/// Invariant (warned on violation, never rejected — operators can override):
///   `safety_timeout_secs > message_wait_for_idle_secs + message_result_secs`
/// so the safety net never fires before the inner sync deadlines under
/// normal conditions.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentToolConfig {
    /// How long to wait for a target agent to become idle before sending it a
    /// new message. After this timeout, `agent(action="ask")` (continue-dialog
    /// path) returns an error instead of blocking forever. Default: 60 seconds.
    #[serde(default = "default_message_wait_for_idle_secs")]
    pub message_wait_for_idle_secs: u64,
    /// How long to wait for a target agent to finish processing the just-sent
    /// message and produce a `last_result`. Used by `agent(action="ask")` for
    /// both spawn-and-wait and continue-and-wait paths. Default: 300 seconds.
    #[serde(default = "default_message_result_secs")]
    pub message_result_secs: u64,
    /// Defense-in-depth outer timeout for any `agent` tool call in the
    /// pipeline-level executor. Should be strictly larger than
    /// `message_wait_for_idle_secs + message_result_secs` so the inner
    /// authoritative timeouts fire first. Default: 600 seconds.
    #[serde(default = "default_safety_timeout_secs")]
    pub safety_timeout_secs: u64,
}

fn default_message_wait_for_idle_secs() -> u64 {
    60
}
fn default_message_result_secs() -> u64 {
    300
}
fn default_safety_timeout_secs() -> u64 {
    600
}

impl Default for AgentToolConfig {
    fn default() -> Self {
        Self {
            message_wait_for_idle_secs: default_message_wait_for_idle_secs(),
            message_result_secs: default_message_result_secs(),
            safety_timeout_secs: default_safety_timeout_secs(),
        }
    }
}

impl AgentToolConfig {
    /// Returns true if the safety net is strictly larger than the maximum
    /// expected inner sync latency (`message_wait_for_idle + message_result`).
    /// Operators that prefer a tighter safety net (e.g. for testing) can
    /// override this — we only emit a warning.
    pub fn invariant_holds(&self) -> bool {
        self.safety_timeout_secs
            > self
                .message_wait_for_idle_secs
                .saturating_add(self.message_result_secs)
    }

    /// Emit a `tracing::warn` if the invariant is violated. Called from
    /// startup and on hot-reload.
    pub fn warn_if_invariant_violated(&self) {
        if !self.invariant_holds() {
            tracing::warn!(
                safety = self.safety_timeout_secs,
                wait_for_idle = self.message_wait_for_idle_secs,
                result = self.message_result_secs,
                "agent_tool: safety_timeout_secs is not strictly greater than \
                 message_wait_for_idle_secs + message_result_secs — outer safety net \
                 may fire before inner deadlines under normal conditions"
            );
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
    /// Postgres Docker container name for pg_dump/pg_restore.
    /// Auto-detected from `docker ps --filter name=postgres` if not set.
    /// Default: "docker-postgres-1"
    #[serde(default = "default_postgres_container")]
    pub postgres_container: String,
}

fn default_backup_cron() -> String {
    "0 0 5 * * *".to_string()
}
fn default_backup_retention_days() -> u32 {
    7
}
fn default_postgres_container() -> String {
    "docker-postgres-1".to_string()
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cron: default_backup_cron(),
            retention_days: default_backup_retention_days(),
            postgres_container: default_postgres_container(),
        }
    }
}

// ── CheckpointConfig ──────────────────────────────────────────────────────────

/// Shadow-git checkpoint snapshots of agent workspace files.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CheckpointConfig {
    /// Enable automatic checkpointing (default: true).
    #[serde(default = "default_checkpoint_enabled")]
    pub enabled: bool,
    /// Maximum number of snapshots to keep per agent (default: 50).
    #[serde(default = "default_checkpoint_keep")]
    pub keep: u32,
    /// Snapshot retention in days — older entries are pruned (default: 14).
    #[serde(default = "default_checkpoint_ttl_days")]
    pub ttl_days: u32,
    /// Path to the shadow-git store (default: "~/.opex/checkpoints/store").
    #[serde(default = "default_checkpoint_store_path")]
    pub store_path: String,
    /// Glob patterns for files to exclude from snapshots (default: []).
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Skip files larger than this many megabytes (default: 5).
    #[serde(default = "default_checkpoint_max_file_size_mb")]
    pub max_file_size_mb: u64,
}

fn default_checkpoint_enabled() -> bool {
    true
}
fn default_checkpoint_keep() -> u32 {
    50
}
fn default_checkpoint_ttl_days() -> u32 {
    14
}
fn default_checkpoint_store_path() -> String {
    "~/.opex/checkpoints/store".to_string()
}
fn default_checkpoint_max_file_size_mb() -> u64 {
    5
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            enabled: default_checkpoint_enabled(),
            keep: default_checkpoint_keep(),
            ttl_days: default_checkpoint_ttl_days(),
            store_path: default_checkpoint_store_path(),
            excludes: Vec::new(),
            max_file_size_mb: default_checkpoint_max_file_size_mb(),
        }
    }
}

// ── CuratorConfig ─────────────────────────────────────────────────────────────

/// Scheduled skill curator configuration.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CuratorConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_curator_cron")]
    pub cron: String,
    #[serde(default = "default_curator_min_idle_minutes")]
    pub min_idle_minutes: u32,
    #[serde(default = "default_curator_stale_after_days")]
    pub stale_after_days: u32,
    #[serde(default = "default_curator_archive_after_days")]
    pub archive_after_days: u32,
    #[serde(default = "default_curator_max_repairs_per_run")]
    pub max_repairs_per_run: u32,
    #[serde(default = "default_curator_agent_name")]
    pub agent_name: String,
}

fn default_curator_cron() -> String {
    "0 3 * * 0".to_string()
}
fn default_curator_min_idle_minutes() -> u32 {
    30
}
fn default_curator_stale_after_days() -> u32 {
    30
}
fn default_curator_archive_after_days() -> u32 {
    90
}
fn default_curator_max_repairs_per_run() -> u32 {
    10
}
fn default_curator_agent_name() -> String {
    "Opex".to_string()
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cron: default_curator_cron(),
            min_idle_minutes: default_curator_min_idle_minutes(),
            stale_after_days: default_curator_stale_after_days(),
            archive_after_days: default_curator_archive_after_days(),
            max_repairs_per_run: default_curator_max_repairs_per_run(),
            agent_name: default_curator_agent_name(),
        }
    }
}

// ── LspConfig ─────────────────────────────────────────────────────────────────

/// Language server (LSP) orchestration configuration.
///
/// Controls the lifecycle of LSP servers spawned by agents (e.g., for V4A-patch,
/// apply_patch, or other code-aware tools). Each agent can spawn up to
/// `max_servers_per_agent` LSP instances.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct LspConfig {
    /// Enable LSP orchestration. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Idle timeout before terminating an LSP server (seconds).
    /// Default: 600 (10 minutes).
    #[serde(default = "default_lsp_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// Request timeout for LSP operations (seconds).
    /// Default: 30 seconds.
    #[serde(default = "default_lsp_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// TTL for marking a broken LSP server as recoverable (seconds).
    /// Default: 120 seconds.
    #[serde(default = "default_lsp_broken_ttl_secs")]
    pub broken_ttl_secs: u64,
    /// Maximum concurrent LSP servers per agent.
    /// Default: 4.
    #[serde(default = "default_lsp_max_servers_per_agent")]
    pub max_servers_per_agent: usize,
}

fn default_lsp_idle_timeout_secs() -> u64 {
    600
}

fn default_lsp_request_timeout_secs() -> u64 {
    30
}

fn default_lsp_broken_ttl_secs() -> u64 {
    120
}

fn default_lsp_max_servers_per_agent() -> usize {
    4
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_timeout_secs: default_lsp_idle_timeout_secs(),
            request_timeout_secs: default_lsp_request_timeout_secs(),
            broken_ttl_secs: default_lsp_broken_ttl_secs(),
            max_servers_per_agent: default_lsp_max_servers_per_agent(),
        }
    }
}

// ── CleanupConfig ─────────────────────────────────────────────────────────────

/// Phase 62 RES-03: batched cleanup tuning for the hourly `session_timeline`
/// prune cron. Both fields have operator-friendly defaults; `retention_days = 0`
/// disables the hourly cleanup entirely.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CleanupConfig {
    /// Retention for `session_timeline` rows in days. `0` disables cleanup.
    /// Default: 7 days.
    #[serde(default = "default_session_timeline_retention_days")]
    pub session_timeline_retention_days: u32,
    /// Rows deleted per batch iteration — keeps lock hold-time short and
    /// autovacuum-friendly. Must be `> 0`. Default: 5000.
    #[serde(default = "default_session_timeline_batch_size")]
    pub session_timeline_batch_size: i64,
    /// Retention for uploads with non-NULL expires_at (tool_output + client_upload).
    /// Permanent rows (agent_icon) are not affected. Default: 30 days.
    #[serde(default = "default_uploads_retention_days")]
    pub uploads_retention_days: u32,
}

fn default_session_timeline_retention_days() -> u32 {
    7
}
fn default_session_timeline_batch_size() -> i64 {
    5000
}
fn default_uploads_retention_days() -> u32 {
    30
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            session_timeline_retention_days: default_session_timeline_retention_days(),
            session_timeline_batch_size: default_session_timeline_batch_size(),
            uploads_retention_days: default_uploads_retention_days(),
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

fn default_drain_timeout_secs() -> u64 {
    30
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            drain_timeout_secs: default_drain_timeout_secs(),
        }
    }
}

// ── ToolCacheConfig ──────────────────────────────────────────────────────────

/// YAML-tool response cache tuning. The cache is a process-wide singleton
/// (`Arc<ToolExecutionContext>`) shared across all agents.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ToolCacheConfig {
    /// Maximum entries in the YAML-tool response cache. Soft cap — at the
    /// limit, ~10 % of oldest entries (min 1) are evicted before insert.
    #[serde(default = "default_tool_cache_max_entries")]
    pub max_entries: usize,
}

fn default_tool_cache_max_entries() -> usize {
    1000
}

impl Default for ToolCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: default_tool_cache_max_entries(),
        }
    }
}

/// OpenTelemetry configuration.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[allow(dead_code)] // fields read only with `otel` feature
pub struct OtelConfig {
    /// Enable OTEL trace export. Also set `OTEL_EXPORTER_OTLP_ENDPOINT` env var.
    #[serde(default)]
    pub enabled: bool,
    /// Service name reported to the collector (default: "opex-core").
    #[serde(default = "default_otel_service")]
    pub service_name: String,
    /// Trace sampling ratio in `[0.0, 1.0]`. `1.0` (default) = sample every
    /// trace, `0.1` = sample 10% by trace_id, `0.0` = drop all. Uses
    /// `TraceIdRatioBased` so the same trace is consistently kept or
    /// dropped across all services that share its trace_id (essential for
    /// cross-process correlation — half-sampled traces look broken in
    /// Jaeger because Toolgate spans are missing while Core spans exist).
    #[serde(default = "default_otel_sampling_ratio")]
    pub sampling_ratio: f64,
}

fn default_otel_service() -> String {
    "opex-core".to_string()
}

/// External model-metadata catalog (context windows, output limits) merged from
/// aggregators like models.dev. Populates the context-window resolution chain
/// between the native provider probe and the name heuristic.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ModelCatalogConfig {
    /// Fetch + use the catalog. When false the catalog stays empty and callers
    /// fall back to native probe / heuristic (no regression).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Background refresh cadence in hours (min 1). Default 24.
    #[serde(default = "default_catalog_refresh_hours")]
    pub refresh_hours: u64,
    /// models.dev catalog endpoint (or a self-hosted mirror).
    #[serde(default = "default_models_dev_url")]
    pub models_dev_url: String,
}

impl Default for ModelCatalogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            refresh_hours: default_catalog_refresh_hours(),
            models_dev_url: default_models_dev_url(),
        }
    }
}

fn default_catalog_refresh_hours() -> u64 {
    24
}

fn default_models_dev_url() -> String {
    "https://models.dev/api.json".to_string()
}
fn default_otel_sampling_ratio() -> f64 {
    1.0
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: default_otel_service(),
            sampling_ratio: default_otel_sampling_ratio(),
        }
    }
}

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
    /// Maximum time (seconds) for a single channel request (LLM loop + tool
    /// calls) before the dispatcher gives up on the turn. 0 = no limit.
    /// Default: 300 (5 minutes). R-TIMEOUT: raised from 180 so core no longer
    /// kills the turn BEFORE the channel adapter's own 300s wait (bridge.ts) —
    /// the previous asymmetry meant tool-heavy / proxied-LLM turns were dropped
    /// (and the session marked terminal) while the adapter was still waiting.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
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
    /// Maximum time (seconds) an agent may block waiting for the user to
    /// respond to a `clarify` tool call. Default: 300 (5 min).
    ///
    /// **Channel constraint:** the channel dispatcher wraps the entire engine
    /// turn (including any clarify wait) in `request_timeout_secs`. Keep
    /// `clarify_timeout_secs ≤ request_timeout_secs` (both default to 300 s)
    /// so the dispatcher never kills a clarify wait prematurely.
    #[serde(default = "default_clarify_timeout")]
    pub clarify_timeout_secs: u64,
}

fn default_max_requests() -> u32 {
    300
}
fn default_max_tool_concurrency() -> u32 {
    10
}
fn default_request_timeout() -> u64 {
    300
}
fn default_max_restore_size_mb() -> u64 {
    500
}
fn default_max_sessions_per_agent() -> u32 {
    500
}
fn default_clarify_timeout() -> u64 {
    300
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_requests_per_minute: default_max_requests(),
            max_tool_concurrency: default_max_tool_concurrency(),
            request_timeout_secs: default_request_timeout(),
            max_restore_size_mb: default_max_restore_size_mb(),
            max_sessions_per_agent: default_max_sessions_per_agent(),
            clarify_timeout_secs: default_clarify_timeout(),
        }
    }
}

pub(crate) fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SubagentsConfig {
    /// Globally enable or disable the `agent` tool. Toggled at runtime via
    /// `PUT /api/config` `{ "subagents_enabled": bool }`.
    #[serde(default = "default_subagents_enabled")]
    pub enabled: bool,
    /// Wall-clock timeout for an in-process subagent invocation. Parsed
    /// by `parse_subagent_timeout` (`"2m"`, `"30s"`).
    #[serde(default = "default_in_process_timeout")]
    pub in_process_timeout: String,
}

fn default_subagents_enabled() -> bool {
    true
}
fn default_in_process_timeout() -> String {
    "2m".to_string()
}

impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            in_process_timeout: default_in_process_timeout(),
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

fn default_max_concurrent() -> u32 {
    5
}

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

fn default_mcp_mode() -> String {
    "on-demand".to_string()
}
fn default_protocol() -> String {
    "mcp".to_string()
}

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
    #[cfg(test)]
    pub fn validate(&self) -> Result<(), String> {
        if self.cooldown_secs == 0 {
            return Err("cooldown_secs must be >= 1".into());
        }
        Ok(())
    }
}

fn default_cooldown_secs() -> u64 {
    60
}

fn default_condition() -> String {
    "default".to_string()
}

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
    /// Optional TTS provider name override. When set, channel actions calling YAML
    /// tools with `channel_action: send_voice` inject `X-Opex-Provider: <name>`
    /// so toolgate routes the synth request to this specific TTS provider instead
    /// of the global `provider_active[tts]`. Each TTS provider's `options.voice`
    /// determines the voice. Useful for per-agent voice personalities.
    #[serde(default)]
    pub tts_provider: Option<String>,
    /// Optional image-generation provider name override. When set, channel actions
    /// calling YAML tools with `channel_action: send_photo` inject
    /// `X-Opex-Provider: <name>` so toolgate routes the generation request
    /// to this specific imagegen provider instead of the global
    /// `provider_active[imagegen]`. Useful for per-agent visual styles.
    #[serde(default)]
    pub imagegen_provider: Option<String>,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// Maximum output tokens for LLM responses. None = provider default.
    pub max_tokens: Option<u32>,
    pub access: Option<AgentAccessConfig>,
    pub heartbeat: Option<HeartbeatConfig>,
    pub tools: Option<AgentToolPolicy>,
    /// Subagent delegation policy (max recursion depth, deny list extensions).
    /// Section can be omitted from TOML — defaults are sane.
    #[serde(default)]
    pub delegation: DelegationConfig,
    pub compaction: Option<CompactionConfig>,
    pub skill_review: Option<SkillReviewConfig>,
    pub session: Option<SessionConfig>,
    /// Maximum number of tools to include in LLM context.
    /// When total tools exceed this limit, the most relevant ones are selected
    /// by keyword matching against the user's message. None = no limit.
    pub max_tools_in_context: Option<usize>,
    /// Multi-provider routing rules. If non-empty, overrides `provider`/`model`.
    /// Evaluated in order — first matching condition is used.
    #[serde(default)]
    pub routing: Vec<ProviderRouteConfig>,
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
    /// Enable Anthropic prompt caching for this agent.
    ///
    /// When `true` AND the resolved provider is Anthropic-typed, the LLM
    /// request stamps `cache_control: ephemeral` breakpoints on the system
    /// message and on the last stable (system/internal) tool. Subsequent
    /// turns within the 5-minute TTL read from cache for those segments.
    ///
    /// Other provider types (openai, google, *-cli) silently ignore this
    /// flag — no error, no behavior change. See CACHE-04.
    ///
    /// Resolution order (in `factory.rs::resolve_provider_from_row`):
    ///   agent TOML `prompt_cache` (this field) → `ProviderOptions.prompt_cache`
    ///   (provider's `options` JSON in DB) → `false`.
    ///
    /// Setting this to `false` explicitly in agent TOML overrides any
    /// `prompt_cache: true` set in the provider options blob.
    #[serde(default)]
    pub prompt_cache: bool,
    /// Hook configuration — policy enforcement and logging.
    pub hooks: Option<HooksConfig>,
    /// Maximum total tokens (input+output) per day. 0 or absent = unlimited.
    #[serde(default)]
    pub daily_budget_tokens: u64,
    /// Maximum number of failover attempts per request when multi-provider
    /// routing is configured. Does NOT count the primary call — a value of 3
    /// means "up to 3 fallbacks after primary failed". Cap exists to prevent
    /// unbounded cascading failures across long fallback chains
    /// (re-added after commit c55b039 → 8d33376 regression).
    #[serde(default = "default_max_failover_attempts")]
    pub max_failover_attempts: u32,
    /// Tool dispatcher configuration — meta-tool for context reduction.
    #[serde(default)]
    pub tool_dispatcher: ToolDispatcherConfig,
}

fn default_max_failover_attempts() -> u32 {
    3
}

/// Per-agent hooks configuration (TOML: `[agent.hooks]`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct HooksConfig {
    /// Log every tool call and result via tracing.
    #[serde(default)]
    pub log_all_tool_calls: bool,
    /// Block these tools silently (no approval prompt, just deny).
    #[serde(default)]
    pub block_tools: Vec<String>,
    /// External HTTP webhooks fired on matching HookEvents.
    #[serde(default)]
    pub webhooks: Vec<WebhookConfig>,
    /// Total wall-clock budget across the whole decision-webhook chain per tool call
    /// (ms). None → 10 000 ms default. Some(0) → no chain budget. Individual hooks
    /// keep their own `timeout_ms`.
    #[serde(default)]
    pub total_webhook_timeout_ms: Option<u64>,
    /// What to do when the chain budget is exceeded. Default: Open (tool proceeds).
    #[serde(default)]
    pub on_chain_timeout: FailureMode,
}

/// Webhook firing mode: fire-and-forget vs. blocking decision gate.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebhookMode {
    /// Fire-and-forget: POST is sent but the agent does not wait for the
    /// response. Default for backward compatibility with existing configs.
    #[default]
    Async,
    /// Decision gate: agent waits for an HTTP 200 JSON reply and acts on it.
    /// Response fields (all optional): `{"decision":"block"|"continue",
    /// "reason":"...", "inject_context":"...", "modified_args":{...},
    /// "transformed_result":"..."}`. Empty `{}` or missing `decision` → continue.
    /// Times out after `timeout_ms`; behaviour on timeout governed by `on_failure`.
    Decision,
}

/// What to do when a `Decision`-mode webhook times out or returns an error.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FailureMode {
    /// Allow the action to proceed (fail-open). Default.
    #[default]
    Open,
    /// Block the action (fail-closed).
    Closed,
}

fn default_hook_timeout_ms() -> u64 {
    3000
}

/// Outbound webhook subscription for hook events (TOML:
/// `[[agent.hooks.webhooks]]`). Fire-and-forget HTTP POST per matching event.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct WebhookConfig {
    /// Destination URL (must be an absolute http/https URL).
    pub url: String,
    /// HookEvent variant names to subscribe to. Valid values:
    /// "BeforeMessage", "AfterResponse", "BeforeToolCall",
    /// "AfterToolResult", "OnError". Unknown names are ignored at fire time.
    #[serde(default)]
    pub events: Vec<String>,
    /// Firing mode: `async` (fire-and-forget, default) or `decision` (blocking gate).
    #[serde(default)]
    pub mode: WebhookMode,
    /// Regex on tool_name (BeforeToolCall/AfterToolResult). None = all tools.
    #[serde(default)]
    pub tool_matcher: Option<String>,
    /// What to do when a Decision-mode webhook times out or errors.
    #[serde(default)]
    pub on_failure: FailureMode,
    /// Timeout for Decision-mode webhooks in milliseconds (default: 3000).
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
    /// true → bypass SSRF resolver (admin opt-in for localhost/LAN hook service).
    #[serde(default)]
    pub allow_internal: bool,
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

fn default_tool_loop_max() -> usize {
    50
}
fn default_tool_loop_warn() -> usize {
    5
}
fn default_tool_loop_break() -> usize {
    10
}
fn default_max_consecutive_failures() -> usize {
    3
}
fn default_max_auto_continues() -> u8 {
    5
}
fn default_max_loop_nudges() -> usize {
    3
}
fn default_ngram_cycle_length() -> usize {
    6
}

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

fn default_approval_timeout() -> u64 {
    300
}

fn default_language() -> String {
    "ru".to_string()
}
fn default_temperature() -> f64 {
    1.0
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AgentAccessConfig {
    /// Access mode: "open" (anyone can use) or "restricted" (owner + approved users only).
    #[serde(default = "default_access_open")]
    pub mode: String,
    /// User ID of the bot owner (auto-allowed in restricted mode).
    pub owner_id: Option<String>,
}

fn default_access_open() -> String {
    "open".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HeartbeatConfig {
    pub cron: String,
    pub timezone: Option<String>,
    /// Channel to announce heartbeat results to (e.g. "telegram"). Uses `owner_id` as `chat_id`.
    pub announce_to: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
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

/// Configuration for subagent delegation behavior.
///
/// Maps to `[agent.delegation]` section in agent TOML config. All fields
/// have defaults — section can be omitted entirely (backward compat with
/// existing TOML files).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelegationConfig {
    /// Maximum recursion depth for `agent` tool spawn.
    /// 0 = top-level agent, 1 = first subagent, etc.
    /// Default 1 means subagents CANNOT spawn further subagents.
    /// Set to 2+ to allow nested delegation (orchestrator pattern).
    #[serde(default = "default_max_depth")]
    pub max_depth: u8,

    /// Tools added on top of SUBAGENT_DENIED_TOOLS (extends, doesn't replace).
    /// Common: `["code_exec"]` to forbid sandbox in subagents.
    #[serde(default)]
    pub blocked_tools_extra: Vec<String>,

    /// When `Some(true|false)`, overrides parent's tool_dispatcher.enabled
    /// for subagents spawned by this agent. When `None`, subagent inherits
    /// parent's setting.
    #[serde(default)]
    pub subagent_dispatcher_enabled: Option<bool>,
}

fn default_max_depth() -> u8 {
    1
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            blocked_tools_extra: Vec::new(),
            subagent_dispatcher_enabled: None,
        }
    }
}

impl DelegationConfig {
    /// Validate delegation policy. Returns a list of error messages (empty if valid).
    /// Called from `AgentConfig::load()` after TOML parse to surface misconfiguration
    /// at startup with full context (agent name) instead of latent runtime DoS.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.max_depth == 0 {
            errors.push(
                "max_depth must be >= 1 (0 disables ALL spawning, including top-level — \
                 read the doc-comment carefully: 0 means \"current depth\", not \"disable nesting\")"
                .to_string()
            );
        }

        // Tool name regex: [a-zA-Z0-9_-]+ (project convention).
        // We warn on violations but don't reject — operator may have YAML/MCP tools not yet loaded.
        // Validation here is best-effort syntactic — semantic existence check happens at filter time.
        let valid_tool_name = |name: &str| -> bool {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };

        for name in self.blocked_tools_extra.iter() {
            if !valid_tool_name(name) {
                errors.push(format!(
                    "invalid tool name {name:?} (expected [a-zA-Z0-9_-]+ — case sensitive)"
                ));
            }
        }

        errors
    }
}

/// Configuration for the tool dispatcher meta-tool (`tool_use`).
///
/// Maps to `[agent.tool_dispatcher]` section in agent TOML config. All fields
/// have defaults — section can be omitted (existing agents preserved).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDispatcherConfig {
    /// When true, the agent emits a compact tools array (static core + tool_use)
    /// and most tools are reachable only via tool_use(action="call", ...).
    /// Default: false (no behaviour change for existing agents).
    #[serde(default)]
    pub enabled: bool,

    /// Extra tool names always kept in the tools array even when enabled.
    /// Subject to deny-list and base-only filtering at context-build time.
    #[serde(default)]
    pub core_extra: Vec<String>,

    /// Cap on auto-promoted-per-session system extension tools.
    /// Hardcoded threshold = 2 successful calls; cap at this value prevents
    /// runaway promotion in long sessions.
    #[serde(default = "default_promotion_max")]
    pub promotion_max: u32,
}

fn default_promotion_max() -> u32 {
    8
}

impl Default for ToolDispatcherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            core_extra: Vec::new(),
            promotion_max: default_promotion_max(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CompactionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    #[serde(default = "default_false")]
    pub preserve_tool_calls: bool,
    #[serde(default = "default_preserve_last_n")]
    pub preserve_last_n: u32,
    /// Override max context tokens (default: auto-detect from model name).
    pub max_context_tokens: Option<u32>,
    /// Head protection: number of messages to always keep (system + first user + first assistant).
    #[serde(default = "CompactionConfig::default_protect_first_n")]
    pub protect_first_n: usize,
    /// Tail token budget as a fraction of threshold_tokens.
    /// tail_budget = (context_limit * threshold * summary_target_ratio) tokens.
    #[serde(default = "CompactionConfig::default_summary_target_ratio")]
    pub summary_target_ratio: f64,
    /// Skip compression if savings < this fraction. Anti-thrashing.
    #[serde(default = "CompactionConfig::default_anti_thrash_min_savings")]
    pub anti_thrash_min_savings: f64,
    /// Stop attempting compression after this many consecutive ineffective compressions.
    #[serde(default = "CompactionConfig::default_anti_thrash_max_skips")]
    pub anti_thrash_max_skips: u8,
    /// Keep OPEX's pgvector fact extraction alongside the Hermes summary.
    #[serde(default = "CompactionConfig::default_extract_to_memory")]
    pub extract_to_memory: bool,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            threshold: default_threshold(),
            preserve_tool_calls: default_false(),
            preserve_last_n: default_preserve_last_n(),
            max_context_tokens: None,
            protect_first_n: Self::default_protect_first_n(),
            summary_target_ratio: Self::default_summary_target_ratio(),
            anti_thrash_min_savings: Self::default_anti_thrash_min_savings(),
            anti_thrash_max_skips: Self::default_anti_thrash_max_skips(),
            extract_to_memory: Self::default_extract_to_memory(),
        }
    }
}

impl CompactionConfig {
    fn default_protect_first_n() -> usize {
        3
    }
    fn default_summary_target_ratio() -> f64 {
        0.20
    }
    fn default_anti_thrash_min_savings() -> f64 {
        0.10
    }
    fn default_anti_thrash_max_skips() -> u8 {
        2
    }
    fn default_extract_to_memory() -> bool {
        true
    }
}

fn default_threshold() -> f64 {
    0.8
}
fn default_preserve_last_n() -> u32 {
    10
}
fn default_false() -> bool {
    false
}

/// Per-agent session skill review config (TOML: `[agent.skill_review]`).
///
/// When enabled, after each `Done` session with ≥ `min_tool_calls` tool
/// invocations, a background task analyzes the session for skill improvements
/// and queues repairs via `pending_skill_repairs`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "SkillReviewConfig::default_min_tool_calls")]
    pub min_tool_calls: u32,
}

impl SkillReviewConfig {
    fn default_min_tool_calls() -> u32 {
        3
    }
}

impl Default for SkillReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_tool_calls: 3,
        }
    }
}

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

fn default_dm_scope() -> String {
    "per-channel-peer".to_string()
}
fn default_session_ttl_days() -> u32 {
    30
}

/// Watchdog configuration for stuck session detection.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WatchdogConfig {
    /// Seconds of inactivity before watchdog kills the session.
    /// Inactivity = no `upsert_streaming_message` or tool result written.
    /// Default: 600 (10 minutes).
    #[serde(default = "default_watchdog_inactivity_secs")]
    pub inactivity_secs: u64,
}

fn default_watchdog_inactivity_secs() -> u64 {
    600
}

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
/// Configured under `[sandbox]` in opex.toml:
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
    /// CPU limit per execution (fractional CPUs, e.g. 1.0 = one core).
    #[serde(default = "default_sandbox_cpu")]
    pub cpu_limit: f64,
    /// Extra volume mounts for agent containers (e.g. "docker/toolgate:/toolgate").
    /// Relative paths are resolved against the project root (workspace parent).
    #[serde(default)]
    pub extra_binds: Vec<String>,
}

fn default_sandbox_image() -> String {
    "python:3.12-slim".to_string()
}
fn default_sandbox_timeout() -> u64 {
    30
}
fn default_sandbox_memory() -> u32 {
    256
}
fn default_sandbox_cpu() -> f64 {
    1.0
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            image: default_sandbox_image(),
            timeout_secs: default_sandbox_timeout(),
            memory_mb: default_sandbox_memory(),
            cpu_limit: default_sandbox_cpu(),
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

fn default_compose_file() -> String {
    "docker/docker-compose.yml".into()
}
fn default_rebuild_timeout() -> u64 {
    300
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            compose_file: default_compose_file(),
            rebuild_allowed: vec![],
            rebuild_timeout_secs: default_rebuild_timeout(),
        }
    }
}

// ── VideoConfig ───────────────────────────────────────────────────────────────

/// Video-summarisation tunables (`[video]` in opex.toml).
///
/// `digest_provider` / `digest_model` let you route the LLM digest step to a
/// different provider than the job-owning agent's configured provider — useful
/// for testing large-context or local models (e.g. ollama) without changing
/// the agent's own connection.  When unset, the worker falls back to the
/// agent's own provider (previous behaviour, no change).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct VideoConfig {
    /// Scene-cut sensitivity for key-frame extraction (0..1 ffmpeg scene score).
    #[serde(default)]
    pub scene_threshold: Option<f64>,
    /// High safety ceiling on extracted frames (NOT a product cap).
    #[serde(default)]
    pub frame_ceiling: Option<u32>,
    /// Liveness guard per job (seconds) — fails a wedged job, not a cap on long video.
    #[serde(default)]
    pub job_timeout_secs: Option<u32>,
    /// v1 video-URL download allowlist (yt-dlp).
    #[serde(default)]
    pub url_allowlist: Vec<String>,
    /// Max screenshots embedded per note.
    #[serde(default)]
    pub note_max_frames: Option<u32>,
    /// Obsidian vault name for the obsidian:// deep link.
    #[serde(default)]
    pub vault_name: Option<String>,
    /// Named provider (from the `providers` DB table) to use for the LLM
    /// digest step.  When absent, falls back to the job agent's own provider.
    ///
    /// Example: `digest_provider = "ollama-local"`
    #[serde(default)]
    pub digest_provider: Option<String>,
    /// Optional model override for the digest provider.  When absent, the
    /// provider's `default_model` is used.
    ///
    /// Example: `digest_model = "qwen3:32b"`
    #[serde(default)]
    pub digest_model: Option<String>,
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
        Self::check_renamed_keys(&content)?;
        let config: Self =
            toml::from_str(&content).with_context(|| "failed to parse config TOML")?;
        Ok(config)
    }

    /// Catch operator-facing key renames before the bare serde error surfaces.
    /// Each entry maps an old key to its new name + the [section] it lives in.
    fn check_renamed_keys(raw_toml: &str) -> Result<()> {
        const RENAMES: &[(&str, &str, &str)] = &[
            // (old key, new key, section)
            (
                "session_events_retention_days",
                "session_timeline_retention_days",
                "[cleanup]",
            ),
            (
                "session_events_batch_size",
                "session_timeline_batch_size",
                "[cleanup]",
            ),
        ];
        for (old, new, section) in RENAMES {
            // Match the old key at the start of a line (allowing leading
            // whitespace), followed by optional spaces and `=`. This avoids
            // false positives from comments or inline strings.
            let found_as_key = raw_toml.lines().any(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with(old)
                    && trimmed
                        .get(old.len()..)
                        .map(|tail| tail.trim_start().starts_with('='))
                        .unwrap_or(false)
            });
            if found_as_key {
                anyhow::bail!(
                    "config error: {section} key `{old}` was renamed to \
                     `{new}` in this release. Update opex.toml.",
                );
            }
        }
        Ok(())
    }
}

impl AgentConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read agent config: {}", path.as_ref().display()))?;
        let config: Self =
            toml::from_str(&content).with_context(|| "failed to parse agent config TOML")?;

        // Validate delegation policy. Errors here block agent load — misconfigured
        // delegation is a security/correctness concern, not a tunable.
        let delegation_errors = config.agent.delegation.validate();
        if !delegation_errors.is_empty() {
            anyhow::bail!(
                "agent {:?}: invalid [agent.delegation] section:\n  - {}",
                config.agent.name,
                delegation_errors.join("\n  - ")
            );
        }

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
pub fn update_service_urls(config_path: &str, toolgate_url: Option<&str>) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
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
            && let Some(mp) = tools.get_mut("toolgate")
        {
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

    let mut doc: toml_edit::DocumentMut = content
        .parse()
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

pub fn update_subagents_enabled(config_path: &str, enabled: bool) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
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
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
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

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;

    Ok(())
}

/// Update [gateway].`public_url` in TOML config file.
pub fn update_public_url(config_path: &str, public_url: &str) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
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

    let mut doc: toml_edit::DocumentMut = content
        .parse()
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

/// Update [curator] section in TOML config file.
#[allow(clippy::too_many_arguments)]
pub fn update_curator_config(
    config_path: &str,
    enabled: Option<bool>,
    cron: Option<&str>,
    min_idle_minutes: Option<u32>,
    stale_after_days: Option<u32>,
    archive_after_days: Option<u32>,
    max_repairs_per_run: Option<u32>,
    agent_name: Option<&str>,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;
    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    if doc.get("curator").is_none() {
        doc["curator"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    if let Some(v) = enabled {
        doc["curator"]["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = cron {
        doc["curator"]["cron"] = toml_edit::value(v);
    }
    if let Some(v) = min_idle_minutes {
        doc["curator"]["min_idle_minutes"] = toml_edit::value(i64::from(v));
    }
    if let Some(v) = stale_after_days {
        doc["curator"]["stale_after_days"] = toml_edit::value(i64::from(v));
    }
    if let Some(v) = archive_after_days {
        doc["curator"]["archive_after_days"] = toml_edit::value(i64::from(v));
    }
    if let Some(v) = max_repairs_per_run {
        doc["curator"]["max_repairs_per_run"] = toml_edit::value(i64::from(v));
    }
    if let Some(v) = agent_name {
        doc["curator"]["agent_name"] = toml_edit::value(v);
    }

    std::fs::write(config_path, doc.to_string())
        .with_context(|| format!("failed to write config: {config_path}"))?;
    Ok(())
}

/// Update [agent_tool] section in TOML config file.
pub fn update_agent_tool_config(
    config_path: &str,
    message_wait_for_idle_secs: Option<u64>,
    message_result_secs: Option<u64>,
    safety_timeout_secs: Option<u64>,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config: {config_path}"))?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .with_context(|| "failed to parse config TOML for editing")?;

    if doc.get("agent_tool").is_none() {
        doc["agent_tool"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    if let Some(v) = message_wait_for_idle_secs {
        doc["agent_tool"]["message_wait_for_idle_secs"] = toml_edit::value(v as i64);
    }

    if let Some(v) = message_result_secs {
        doc["agent_tool"]["message_result_secs"] = toml_edit::value(v as i64);
    }

    if let Some(v) = safety_timeout_secs {
        doc["agent_tool"]["safety_timeout_secs"] = toml_edit::value(v as i64);
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
///
/// The `cancel` token is polled every 250 ms so the OS thread exits promptly on
/// graceful shutdown instead of blocking the process indefinitely (Bug 13).
pub fn spawn_config_watcher(
    config_path: String,
    shared: SharedConfig,
    api_write_flag: ConfigApiWriteFlag,
    cancel: tokio_util::sync::CancellationToken,
) {
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

        loop {
            if cancel.is_cancelled() {
                tracing::debug!("config watcher: shutdown signal received, exiting");
                break;
            }

            let event = match rx.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(ev) => ev,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };

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
                            new_config.agent_tool.warn_if_invariant_violated();
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

/// Wrapper for the [agent] section in opex.toml (global defaults).
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
        let cfg: AgentConfig =
            toml::from_str(toml_str).expect("failed to parse minimal AgentConfig");
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
                tts_provider: None,
                imagegen_provider: None,
                temperature: 0.7,
                max_tokens: None,
                access: Some(AgentAccessConfig {
                    mode: "restricted".into(),
                    owner_id: Some("12345".into()),
                }),
                heartbeat: None,
                tools: None,
                delegation: DelegationConfig::default(),
                compaction: Some(CompactionConfig {
                    enabled: true,
                    threshold: 0.9,
                    preserve_tool_calls: true,
                    preserve_last_n: 5,
                    max_context_tokens: Some(8000),
                    protect_first_n: 3,
                    summary_target_ratio: 0.20,
                    anti_thrash_min_savings: 0.10,
                    anti_thrash_max_skips: 2,
                    extract_to_memory: true,
                }),
                skill_review: None,
                session: Some(SessionConfig {
                    dm_scope: "shared".into(),
                    ttl_days: 7,
                    max_messages: 100,
                    prune_tool_output_after_turns: None,
                }),
                max_tools_in_context: Some(20),
                max_history_messages: None,
                prompt_cache: false,
                routing: vec![ProviderRouteConfig {
                    condition: "default".into(),
                    connection: Some("minimax-default".into()),
                    model: Some("m2.5".into()),
                    temperature: Some(0.8),
                    cooldown_secs: 60,
                }],
                approval: None,
                tool_loop: None,
                base: false,
                watchdog: None,
                provider_connection: None,
                fallback_provider: None,
                hooks: None,
                daily_budget_tokens: 0,
                max_failover_attempts: default_max_failover_attempts(),
                tool_dispatcher: ToolDispatcherConfig::default(),
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
                tts_provider: None,
                imagegen_provider: None,
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
                delegation: DelegationConfig::default(),
                compaction: None,
                skill_review: None,
                session: None,
                max_tools_in_context: None,
                max_history_messages: None,
                prompt_cache: false,
                routing: vec![],
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
                max_failover_attempts: default_max_failover_attempts(),
                tool_dispatcher: ToolDispatcherConfig::default(),
            },
        };

        let toml_str = original.to_toml().expect("serialize failed");
        let restored: AgentConfig =
            toml::from_str(&toml_str).expect("deserialize roundtrip failed");
        assert_eq!(original, restored);
    }

    // ── CACHE-01: prompt_cache field deserialization ──

    #[test]
    fn agent_settings_prompt_cache_defaults_to_false() {
        // Legacy agent TOMLs that pre-date Phase 68 must continue to parse.
        // CACHE-01: `prompt_cache` defaults to `false` via #[serde(default)].
        let toml_str = r#"
[agent]
name = "test"
provider = "minimax"
model = "m2.5"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parse minimal AgentConfig");
        assert!(
            !cfg.agent.prompt_cache,
            "absent field must default to false"
        );
    }

    #[test]
    fn agent_settings_prompt_cache_parsed_when_true() {
        // CACHE-01: explicit `prompt_cache = true` in agent TOML must be honored.
        let toml_str = r#"
[agent]
name = "test"
provider = "minimax"
model = "m2.5"
prompt_cache = true
"#;
        let cfg: AgentConfig =
            toml::from_str(toml_str).expect("parse AgentConfig with prompt_cache");
        assert!(cfg.agent.prompt_cache, "explicit true must be parsed");
    }

    // ── 4. LimitsConfig defaults ──

    #[test]
    fn limits_config_defaults() {
        let cfg = LimitsConfig::default();
        assert_eq!(cfg.max_requests_per_minute, 300);
        assert_eq!(cfg.max_tool_concurrency, 10);
        assert_eq!(cfg.request_timeout_secs, 300);
        // Phase 64 SEC-04: new [limits] key — default 500 MB.
        assert_eq!(cfg.max_restore_size_mb, 500);
    }

    // ── 4d. UploadsConfig defaults (Phase 64 SEC-03) ──

    #[test]
    fn uploads_config_defaults() {
        let cfg = UploadsConfig::default();
        assert_eq!(cfg.signed_url_ttl_secs, 86_400);
        assert!(
            cfg.require_signature,
            "v0.26.0 enforces signatures by default"
        );
        assert_eq!(
            cfg.max_upload_bytes, 52_428_800,
            "default 50 MB per-file ceiling"
        );
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
max_upload_bytes = 41943040
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.uploads.signed_url_ttl_secs, 3600);
        assert!(cfg.uploads.require_signature);
        assert_eq!(cfg.uploads.max_upload_bytes, 41_943_040);
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
        assert!(cfg.uploads.require_signature);
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
        assert_eq!(
            cfg.limits.max_restore_size_mb, 500,
            "missing key uses default"
        );
    }

    // ── 5. SubagentsConfig defaults ──

    #[test]
    fn subagents_config_defaults() {
        let cfg = SubagentsConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.in_process_timeout, "2m");
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
        assert_eq!(cfg.protect_first_n, 3);
        assert!((cfg.summary_target_ratio - 0.20).abs() < 0.001);
        assert!((cfg.anti_thrash_min_savings - 0.10).abs() < 0.001);
        assert_eq!(cfg.anti_thrash_max_skips, 2);
        assert!(cfg.extract_to_memory);
    }

    #[test]
    fn compaction_config_new_fields_have_defaults() {
        let cfg: CompactionConfig = toml::from_str("enabled = true").unwrap();
        assert_eq!(cfg.protect_first_n, 3);
        assert!((cfg.summary_target_ratio - 0.20).abs() < 0.001);
        assert!((cfg.anti_thrash_min_savings - 0.10).abs() < 0.001);
        assert_eq!(cfg.anti_thrash_max_skips, 2);
        assert!(cfg.extract_to_memory);
    }

    // ── 8. SkillReviewConfig ──

    #[test]
    fn skill_review_config_defaults() {
        let cfg = SkillReviewConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.min_tool_calls, 3);
    }

    #[test]
    fn skill_review_config_from_toml() {
        let toml_str = r#"
            [agent]
            name = "Test"
            provider = "openai"
            model = "gpt-4o"
            [agent.skill_review]
            enabled = true
            min_tool_calls = 5
        "#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
        let sr = cfg.agent.skill_review.expect("skill_review present");
        assert!(sr.enabled);
        assert_eq!(sr.min_tool_calls, 5);
    }

    #[test]
    fn skill_review_absent_gives_none() {
        let toml_str = r#"
            [agent]
            name = "Test"
            provider = "openai"
            model = "gpt-4o"
        "#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
        assert!(cfg.agent.skill_review.is_none());
    }

    #[test]
    fn skill_review_enabled_only_gives_default_min_tool_calls() {
        let toml_str = r#"
            [agent]
            name = "Test"
            provider = "openai"
            model = "gpt-4o"
            [agent.skill_review]
            enabled = true
        "#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
        let sr = cfg.agent.skill_review.expect("present");
        assert!(sr.enabled);
        assert_eq!(sr.min_tool_calls, 3);
    }

    // ── 9. SessionConfig defaults ──

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
        assert_eq!(cfg.subagents.in_process_timeout, "2m");
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
url = "postgres://user:pass@db:5432/opex"

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
        let cfg: AppConfig =
            toml::from_str(toml_str).expect("failed to parse AppConfig with overrides");
        assert_eq!(cfg.gateway.listen, "127.0.0.1:9999");
        assert_eq!(cfg.gateway.auth_token_env.as_deref(), Some("MY_TOKEN"));
        assert_eq!(cfg.database.url, "postgres://user:pass@db:5432/opex");
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
        let entry: McpFileEntry =
            serde_yaml::from_str(yaml_str).expect("failed to parse minimal McpFileEntry");

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
        assert_eq!(
            cfg.agent.routing[0].connection.as_deref(),
            Some("openai-default")
        );
        assert_eq!(cfg.agent.routing[0].model.as_deref(), Some("gpt-4"));
        assert!(cfg.agent.routing[0].temperature.is_none());
    }

    // ── connection-based route tests ──────────────────────────────────────────

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
        assert!(tools.groups.skill_editing); // default true
        assert!(!tools.groups.session_tools);
    }

    // ── AgentToolConfig (multi-agent timeouts) ──────────────────────────────

    /// Empty TOML must produce all three defaults (60/300/600).
    #[test]
    fn agent_tool_config_defaults_when_missing() {
        let toml_str = r#""#;
        let cfg: AgentToolConfig = toml::from_str(toml_str).expect("empty must parse to defaults");
        assert_eq!(cfg.message_wait_for_idle_secs, 60);
        assert_eq!(cfg.message_result_secs, 300);
        assert_eq!(cfg.safety_timeout_secs, 600);
        assert!(cfg.invariant_holds(), "default invariant must hold");
    }

    /// AppConfig parses without an `[agent_tool]` section and falls back to defaults.
    #[test]
    fn app_config_agent_tool_section_defaults() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("parse minimal");
        assert_eq!(cfg.agent_tool.message_wait_for_idle_secs, 60);
        assert_eq!(cfg.agent_tool.message_result_secs, 300);
        assert_eq!(cfg.agent_tool.safety_timeout_secs, 600);
    }

    /// Operator-set values are picked up cleanly.
    #[test]
    fn agent_tool_config_custom_values() {
        let toml_str = r#"
[agent_tool]
message_wait_for_idle_secs = 30
message_result_secs = 600
safety_timeout_secs = 1200
"#;
        #[derive(serde::Deserialize)]
        struct Wrapper {
            agent_tool: AgentToolConfig,
        }
        let w: Wrapper = toml::from_str(toml_str).expect("custom values");
        assert_eq!(w.agent_tool.message_wait_for_idle_secs, 30);
        assert_eq!(w.agent_tool.message_result_secs, 600);
        assert_eq!(w.agent_tool.safety_timeout_secs, 1200);
        assert!(w.agent_tool.invariant_holds());
    }

    /// Partial section: only one field present, others fall back to per-field defaults.
    #[test]
    fn agent_tool_config_partial_uses_per_field_defaults() {
        let toml_str = r#"
message_result_secs = 900
"#;
        let cfg: AgentToolConfig = toml::from_str(toml_str).expect("parse partial");
        assert_eq!(cfg.message_wait_for_idle_secs, 60);
        assert_eq!(cfg.message_result_secs, 900);
        assert_eq!(cfg.safety_timeout_secs, 600);
        // 600 < 60 + 900 = 960 → invariant violated, but config still loads.
        assert!(!cfg.invariant_holds());
    }

    /// Invariant violation: warn-only path (just verifies the predicate without
    /// rejecting). `warn_if_invariant_violated` is exercised here purely so the
    /// branch is covered; it logs via `tracing` and returns `()`.
    #[test]
    fn agent_tool_config_invariant_violation_does_not_panic() {
        let cfg = AgentToolConfig {
            message_wait_for_idle_secs: 100,
            message_result_secs: 500,
            safety_timeout_secs: 300, // less than 100 + 500 = 600
        };
        assert!(!cfg.invariant_holds());
        cfg.warn_if_invariant_violated(); // must not panic
    }

    /// Unknown fields are rejected (defense against typos).
    #[test]
    fn agent_tool_config_rejects_unknown_fields() {
        let toml_str = r#"
foo_bar_baz = 42
"#;
        let result: Result<AgentToolConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err(), "deny_unknown_fields must reject typo");
    }

    // ── DelegationConfig validation ──

    #[test]
    fn delegation_validate_max_depth_zero_rejected() {
        let cfg = DelegationConfig {
            max_depth: 0,
            blocked_tools_extra: vec![],
            subagent_dispatcher_enabled: None,
        };
        let errors = cfg.validate();
        assert!(!errors.is_empty(), "max_depth=0 must be rejected");
        assert!(errors[0].contains("max_depth must be >= 1"));
    }

    #[test]
    fn delegation_validate_invalid_tool_name_rejected() {
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec!["valid_tool".into(), "bad name".into()],
            subagent_dispatcher_enabled: None,
        };
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("invalid tool name")));
        assert!(errors.iter().any(|e| e.contains("\"bad name\"")));
    }

    #[test]
    fn delegation_validate_default_is_valid() {
        let cfg = DelegationConfig::default();
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn delegation_validate_typical_config_is_valid() {
        let cfg = DelegationConfig {
            max_depth: 2,
            blocked_tools_extra: vec!["code_exec".into(), "process".into()],
            subagent_dispatcher_enabled: None,
        };
        assert!(cfg.validate().is_empty());
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

#[cfg(test)]
mod curator_config_tests {
    use super::*;

    #[test]
    fn curator_config_defaults() {
        let cfg: CuratorConfig = Default::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.cron, "0 3 * * 0");
        assert_eq!(cfg.min_idle_minutes, 30);
        assert_eq!(cfg.stale_after_days, 30);
        assert_eq!(cfg.archive_after_days, 90);
        assert_eq!(cfg.max_repairs_per_run, 10);
    }
}

#[cfg(test)]
mod backup_config_tests {
    use super::*;

    #[test]
    fn backup_config_default_postgres_container() {
        let cfg = BackupConfig::default();
        assert_eq!(cfg.postgres_container, "docker-postgres-1");
    }

    #[test]
    fn backup_config_parses_postgres_container_from_toml() {
        let cfg: AppConfig = toml::from_str(
            r#"
            [gateway]
            listen = "0.0.0.0:18789"
            [database]
            url = "postgres://localhost/test"
            [backup]
            postgres_container = "my-postgres-2"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.backup.postgres_container, "my-postgres-2");
    }
}

#[cfg(test)]
mod checkpoint_config_tests {
    use super::*;

    #[test]
    fn checkpoint_config_defaults() {
        let c = CheckpointConfig::default();
        assert!(c.enabled);
        assert_eq!(c.keep, 50);
        assert_eq!(c.ttl_days, 14);
        assert_eq!(c.max_file_size_mb, 5);
        assert_eq!(c.store_path, "~/.opex/checkpoints/store");
        assert!(c.excludes.is_empty());
    }

    #[test]
    fn checkpoint_config_parses_from_toml() {
        let toml = r#"
            enabled = false
            keep = 10
            ttl_days = 3
            store_path = "/tmp/cp"
            excludes = ["foo", "bar"]
            max_file_size_mb = 2
        "#;
        let c: CheckpointConfig = toml::from_str(toml).unwrap();
        assert!(!c.enabled);
        assert_eq!(c.keep, 10);
        assert_eq!(c.excludes, vec!["foo".to_string(), "bar".to_string()]);
    }
}

#[cfg(test)]
mod lsp_config_tests {
    use super::*;

    #[test]
    fn lsp_config_defaults() {
        let cfg: LspConfig = Default::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.idle_timeout_secs, 600);
        assert_eq!(cfg.request_timeout_secs, 30);
        assert_eq!(cfg.broken_ttl_secs, 120);
        assert_eq!(cfg.max_servers_per_agent, 4);
    }

    #[test]
    fn lsp_config_parses_enabled_from_toml() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[lsp]
enabled = true
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("failed to parse AppConfig");
        assert!(cfg.lsp.enabled);
        assert_eq!(cfg.lsp.request_timeout_secs, 30);
        assert_eq!(cfg.lsp.idle_timeout_secs, 600);
        assert_eq!(cfg.lsp.broken_ttl_secs, 120);
        assert_eq!(cfg.lsp.max_servers_per_agent, 4);
    }

    #[test]
    fn lsp_config_fully_empty_config() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("failed to parse AppConfig");
        assert!(!cfg.lsp.enabled);
        assert_eq!(cfg.lsp.request_timeout_secs, 30);
        assert_eq!(cfg.lsp.max_servers_per_agent, 4);
    }

    #[test]
    fn lsp_config_parses_all_fields_from_toml() {
        let toml_str = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[lsp]
enabled = true
idle_timeout_secs = 300
request_timeout_secs = 45
broken_ttl_secs = 60
max_servers_per_agent = 8
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("failed to parse AppConfig");
        assert!(cfg.lsp.enabled);
        assert_eq!(cfg.lsp.idle_timeout_secs, 300);
        assert_eq!(cfg.lsp.request_timeout_secs, 45);
        assert_eq!(cfg.lsp.broken_ttl_secs, 60);
        assert_eq!(cfg.lsp.max_servers_per_agent, 8);
    }
}

#[cfg(test)]
mod precheck_tests {
    use super::{AppConfig, FailureMode, WebhookConfig, WebhookMode};
    use std::io::Write;

    fn write_temp_toml(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("temp file");
        f.write_all(content.as_bytes()).expect("write toml");
        f
    }

    #[test]
    fn precheck_rejects_old_session_events_retention_days() {
        let toml = r#"
[cleanup]
session_events_retention_days = 14
"#;
        let f = write_temp_toml(toml);
        let err = AppConfig::load(f.path()).expect_err("must reject old key");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("session_events_retention_days")
                && msg.contains("renamed")
                && msg.contains("session_timeline_retention_days"),
            "PreCheck error must name old AND new key. Got: {msg}"
        );
    }

    #[test]
    fn precheck_rejects_old_session_events_batch_size() {
        let toml = r#"
[cleanup]
session_events_batch_size = 1000
"#;
        let f = write_temp_toml(toml);
        let err = AppConfig::load(f.path()).expect_err("must reject old key");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("session_events_batch_size")
                && msg.contains("session_timeline_batch_size"),
            "PreCheck error must name old AND new key. Got: {msg}"
        );
    }

    #[test]
    fn precheck_accepts_new_session_timeline_keys() {
        let toml = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[cleanup]
session_timeline_retention_days = 14
session_timeline_batch_size = 1000
"#;
        let f = write_temp_toml(toml);
        let cfg = AppConfig::load(f.path()).expect("new keys must parse");
        assert_eq!(cfg.cleanup.session_timeline_retention_days, 14);
        assert_eq!(cfg.cleanup.session_timeline_batch_size, 1000);
    }

    #[test]
    fn webhook_config_backward_compat_defaults_async() {
        let toml = r#"url = "https://x/h"
events = ["BeforeToolCall"]"#;
        let w: WebhookConfig = toml::from_str(toml).unwrap();
        assert!(matches!(w.mode, WebhookMode::Async));
        assert!(matches!(w.on_failure, FailureMode::Open));
        assert_eq!(w.timeout_ms, 3000);
        assert!(!w.allow_internal);
        assert!(w.tool_matcher.is_none());
    }

    #[test]
    fn webhook_config_decision_parses() {
        let toml = r#"url = "https://x/h"
events = ["BeforeToolCall"]
mode = "decision"
tool_matcher = "code_exec|workspace_.*"
on_failure = "closed"
timeout_ms = 1500
allow_internal = true"#;
        let w: WebhookConfig = toml::from_str(toml).unwrap();
        assert!(matches!(w.mode, WebhookMode::Decision));
        assert!(matches!(w.on_failure, FailureMode::Closed));
        assert_eq!(w.timeout_ms, 1500);
        assert!(w.allow_internal);
        assert_eq!(w.tool_matcher.as_deref(), Some("code_exec|workspace_.*"));
    }

    #[test]
    fn video_config_digest_provider_and_model_round_trip() {
        let toml = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[video]
digest_provider = "ollama-local"
digest_model = "qwen3:32b"
"#;
        let f = write_temp_toml(toml);
        let cfg = AppConfig::load(f.path()).expect("video config must parse");
        assert_eq!(cfg.video.digest_provider.as_deref(), Some("ollama-local"));
        assert_eq!(cfg.video.digest_model.as_deref(), Some("qwen3:32b"));
    }

    #[test]
    fn video_config_digest_fields_default_to_none() {
        let toml = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"
"#;
        let f = write_temp_toml(toml);
        let cfg = AppConfig::load(f.path()).expect("minimal config must parse");
        assert!(cfg.video.digest_provider.is_none(), "digest_provider defaults to None");
        assert!(cfg.video.digest_model.is_none(), "digest_model defaults to None");
    }

    #[test]
    fn video_config_legacy_keys_still_parse() {
        // Ensure existing [video] sections with old numeric keys still parse
        // (no deny_unknown_fields on VideoConfig — forward/backward compat).
        let toml = r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[video]
scene_threshold = 0.4
frame_ceiling = 200
job_timeout_secs = 1800
note_max_frames = 24
vault_name = "zettelkasten"
digest_provider = "my-provider"
"#;
        let f = write_temp_toml(toml);
        let cfg = AppConfig::load(f.path()).expect("video config with all keys must parse");
        assert_eq!(cfg.video.digest_provider.as_deref(), Some("my-provider"));
        assert!(cfg.video.digest_model.is_none());
        assert_eq!(cfg.video.vault_name.as_deref(), Some("zettelkasten"));
    }
}

#[cfg(test)]
mod semantic_cache_tests {
    use super::*;

    #[test]
    fn defaults_cover_the_four_builtin_tools() {
        let cfg = SemanticCacheConfig::default();
        let s = cfg.for_tool("searxng_search").expect("searxng is cacheable by default");
        assert_eq!(s.ttl_secs, 3600);
        assert!((s.threshold - 0.95).abs() < f32::EPSILON);
        assert!(cfg.for_tool("web_search").is_some());
    }

    #[test]
    fn override_replaces_default_ttl() {
        let mut map = std::collections::HashMap::new();
        map.insert("web_search".to_string(), SemanticCacheToolConfig { ttl_secs: 300, threshold: 0.9 });
        let cfg = SemanticCacheConfig { tools: map };
        assert_eq!(cfg.for_tool("web_search").unwrap().ttl_secs, 300);
        // built-in tools NOT in the override map still resolve to defaults
        assert_eq!(cfg.for_tool("searxng_search").unwrap().ttl_secs, 3600);
    }

    #[test]
    fn unknown_tool_is_not_cacheable() {
        assert!(SemanticCacheConfig::default().for_tool("workspace_read").is_none());
    }
}
