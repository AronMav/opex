

export interface StatusInfo {
  status: string;
  version: string;
  uptime_seconds: number;
  db: boolean;
  listen: string;
  agents: string[];
  memory_chunks: number;
  scheduled_jobs: number;
  active_sessions: number;
  tools_registered: number;
}

export interface StatsInfo {
  messages_today: number;
  sessions_today: number;
  total_messages: number;
  total_sessions: number;
  recent_sessions?: { id: string; agent_id: string; channel: string; last_message_at: string; title: string | null }[];
}

// AgentInfo is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/agents/dto_structs.rs
// Regenerate: make gen-types
export type { AgentInfoDto as AgentInfo } from "./api.generated";

export interface RoutingRule {
  provider: string;
  model: string;
  condition: string;
  base_url?: string | null;
  api_key_env?: string | null;
  api_key_envs?: string[];
  temperature?: number | null;
  max_tokens?: number | null;
  prompt_cache?: boolean;
  cooldown_secs?: number;
}

// AgentDetail is now generated from Rust DTOs via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/agents/dto.rs
// Regenerate: make gen-types
export type { AgentDetailDto as AgentDetail } from "./api.generated";

// SessionRow is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/db/sessions.rs
// Regenerate: make gen-types
import type { Session as _Session } from "./api.generated";
// last_input_tokens and segment_count are appended server-side (not in the ts-rs DTO).
export type SessionRow = _Session & {
  last_input_tokens?: number | null;
  segment_count?: number;
};

// MessageRow is now generated from Rust DTO via ts-rs codegen.
// Note: feedback is number | null (DB-accurate); the old type had number (incorrect).
// Source: crates/opex-core/src/db/sessions.rs
// Regenerate: make gen-types
export type { MessageRow } from "./api.generated";

// Messages pagination with compression divider events.
export interface CompressionEvent {
  segment_index: number;
  first_live_message_id: string;
  summary: string;
}

export interface MessagesResponse {
  messages: import("./api.generated").MessageRow[];
  compression_events: CompressionEvent[];
  has_more: boolean;
}

// CronJob is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/cron_dto_structs.rs
// Regenerate: make gen-types
export type { CronJobDto as CronJob } from "./api.generated";

// CronRun is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/cron_dto_structs.rs
// Regenerate: make gen-types
export type { CronRunDto as CronRun } from "./api.generated";

// MemoryDocument is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/memory_dto_structs.rs
// Regenerate: make gen-types
export type { MemoryDocumentDto as MemoryDocument } from "./api.generated";

// MemoryStats is now generated from Rust DTO via ts-rs codegen.
// Drift fix: tasks field (pending/processing/done/failed) was emitted by handler but absent from TS type.
// Source: crates/opex-core/src/gateway/handlers/memory_dto_structs.rs
// Regenerate: make gen-types
export type { MemoryStatsDto as MemoryStats } from "./api.generated";

// ToolEntry is now generated from Rust DTO via ts-rs codegen.
// Drift fixes: concurrency_limit is number (not number | null); managed is boolean (not optional).
// Source: crates/opex-core/src/gateway/handlers/tools_dto_structs.rs
// Regenerate: make gen-types
export type { ToolEntryDto as ToolEntry } from "./api.generated";

export interface SkillEntry {
  name: string;
  description: string;
  triggers: string[];
  tools_required: string[];
  priority: number;
  instructions_len: number;
  state: "active" | "stale" | "archived";
  last_used_at: string | null;
  pinned?: boolean;
}

export interface SkillVersion {
  id: string;
  skill_name: string;
  generation: number;
  evolution_type: string;
  trigger_reason: string | null;
  content: string;
  content_hash: string;
  created_at: string;
}

export interface CuratorStatus {
  enabled: boolean;
  cron: string;
  last_run_at: string | null;
  last_run_id: string | null;
  last_phase1: number;
  last_phase2: number;
  last_phase3: number;
}

export interface CuratorConfig {
  enabled: boolean;
  cron: string;
  min_idle_minutes: number;
  stale_after_days: number;
  archive_after_days: number;
  max_repairs_per_run: number;
  agent_name: string;
}

export interface CuratorRun {
  id: string;
  started_at: string;
  finished_at: string | null;
  duration_ms: number | null;
  triggered_by: string;
  phase1_transitions: number;
  phase2_repairs: number;
  phase3_commands: number;
  skipped_reason: string | null;
  report_md: string | null;
  error: string | null;
}

export interface CuratorDecision {
  action: "archive" | "reject" | "fix";
  reason: string | null;
  decided_at: string;
}

export interface SkillCuratorDecisions {
  decisions: Array<CuratorDecision & { id: number; skill_name: string }>;
}

export interface YamlToolEntry {
  name: string;
  description: string;
  endpoint: string;
  method: string;
  status: "verified" | "draft" | "disabled";
  parameters_count: number;
  tags: string[];
}

// McpEntry is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/tools_dto_structs.rs
// Regenerate: make gen-types
export type { McpEntryDto as McpEntry } from "./api.generated";

export interface FileEntry {
  name: string;
  is_dir: boolean;
  display: string;
}

export type WorkspaceFile =
  | { content: string; path: string; is_dir: false }
  | { is_binary: true; mime: string; size: number; url: string; path: string; is_dir: false };

export interface SecretInfo {
  name: string;
  scope: string;
  description: string | null;
  has_value: boolean;
  created_at: string;
  updated_at: string;
}

export type { GitHubRepo as GitHubRepoInfo } from "./api.generated";

export interface OAuthAccount {
  id: string;
  provider: string;
  display_name: string;
  user_email: string | null;
  scope: string;
  status: string;
  expires_at: string | null;
  connected_at: string | null;
  created_at: string;
}

export interface OAuthBinding {
  agent_id: string;
  provider: string;
  account_id: string;
  display_name: string;
  user_email: string | null;
  status: string;
  expires_at: string | null;
  connected_at: string | null;
  bound_at: string;
}

export interface LogEntry {
  level: string;
  message: string;
  target?: string;
  timestamp: string;
}

export interface UsageSummary {
  agent_id: string;
  provider: string;
  model: string;
  total_input: number;
  total_output: number;
  call_count: number;
  estimated_cost: number | null;
}

export interface UsageResponse {
  ok: boolean;
  days: number;
  usage: UsageSummary[];
}

export interface DailyUsageEntry {
  date: string;
  agent_id: string;
  provider: string;
  model: string;
  input_tokens: number;
  output_tokens: number;
  call_count: number;
}

export interface DailyUsageResponse {
  ok: boolean;
  days: number;
  daily: DailyUsageEntry[];
}



export interface AuditEvent {
  id: string;
  agent_id: string;
  event_type: string;
  actor: string | null;
  details: Record<string, unknown>;
  created_at: string;
}

// ChannelRow is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/channels_dto_structs.rs
// Regenerate: make gen-types
export type { ChannelRowDto as ChannelRow } from "./api.generated";

// ActiveChannel is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/channels_dto_structs.rs
// Regenerate: make gen-types
export type { ActiveChannelDto as ActiveChannel } from "./api.generated";

// BackupEntry is now generated from Rust DTO via ts-rs codegen.
// Drift fix: created_at is string | null (filesystem mtime can be absent).
// Source: crates/opex-core/src/gateway/handlers/backup_dto_structs.rs
// Regenerate: make gen-types
export type { BackupEntryDto as BackupEntry } from "./api.generated";

// WebhookEntry is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/webhooks_dto_structs.rs
// Regenerate: make gen-types
export type { WebhookEntryDto as WebhookEntry } from "./api.generated";

// ApprovalEntry is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/gateway/handlers/agents/approvals_dto_structs.rs
// Regenerate: make gen-types
export type { ApprovalEntryDto as ApprovalEntry } from "./api.generated";

export interface ProviderType {
  id: string;
  name: string;
  default_base_url: string;
  chat_path: string;
  default_secret_name: string;
  requires_api_key: boolean;
  supports_model_listing: boolean;
}

export type TimeoutsConfig = {
  connect_secs: number;              // 1..=120
  request_secs: number;              // 0..=3600, 0 = no limit
  stream_inactivity_secs: number;    // 0..=3600, 0 = no limit
  stream_max_duration_secs: number;  // 0..=7200, 0 = no limit
};

export type ProviderOptions = {
  timeouts?: Partial<TimeoutsConfig>;
  api_key_envs?: string[];
  max_retries?: number;  // 1..=10, default 3
  // Per-model explicit context windows (tokens), keyed by model id. Set entries
  // for models whose provider API doesn't expose the window (e.g. MiMo). Models
  // absent from the map ⇒ auto-detect via provider API, then name heuristic.
  // Each value must be >= 1000 (tokens, not thousands).
  context_windows?: Record<string, number>;
  // Unknown fields land here — UI will preserve them on round-trip.
  [extra: string]: unknown;
};

export interface Provider {
  id: string;
  name: string;
  type: string;
  provider_type: string;
  base_url: string | null;
  default_model: string | null;
  has_api_key: boolean;
  api_key: string | null;
  enabled: boolean;
  options: ProviderOptions;
  notes: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateProviderInput {
  name: string;
  type: string;
  provider_type: string;
  base_url?: string;
  api_key?: string;
  default_model?: string;
  enabled?: boolean;
  options?: ProviderOptions;
  notes?: string;
}

export interface ProviderActiveRow {
  capability: string;
  provider_name: string | null;
  priority: number;
}

export interface MediaDriverInfo {
  driver: string;
  label: string;
  requires_key: boolean;
}

// NotificationRow is now generated from Rust DTO via ts-rs codegen.
// Source: crates/opex-core/src/db/notifications.rs
// Regenerate: make gen-types
export type { Notification as NotificationRow } from "./api.generated";

// NotificationsResponse is now generated. `items` is required (not optional);
// the stale `notifications?` key has been removed.
// Regenerate: make gen-types
export type { NotificationsResponseDto as NotificationsResponse } from "./api.generated";

// `NotificationRow.type` is a plain `string` in the generated DTO (the Rust
// `Notification` struct stores `notification_type` as free-form text, not an
// enum) — there is no literal union to widen for new event kinds. Known
// values in use across the UI: "access_request" | "tool_approval" |
// "agent_error" | "watchdog_alert" | "initiative_proposal" (Stage C
// self-proposed goals — see notification-bell.tsx's getNotificationRoute)
// plus the media-flavoured "tts_*"/"image_*"/"video_*"/"media_*" events.

// ── Agent Plan (Stage C initiative) ─────────────────────────────────────────
// Hand-written — the handler returns an ad-hoc serde_json::json!(...) object,
// not a ts-rs DTO, so there is nothing to regenerate here.
// Source: crates/opex-core/src/gateway/handlers/agents/initiative.rs
//         crates/opex-core/src/db/agent_plans.rs (Proposal)

export interface AgentPlanProposal {
  id: string;
  text: string;
  status: "pending" | "approved" | "dismissed";
  created_at: string;
  acted_at: string | null;
}

export interface AgentPlanActiveGoal {
  goal: string;
  turns: number;
  session_id: string;
}

export interface AgentPlan {
  agent: string;
  current_focus: string | null;
  proposals: AgentPlanProposal[];
  active_goals: AgentPlanActiveGoal[];
}

export interface TaskStep {
  id: string;
  title: string;
  status: "pending" | "in_progress" | "done" | "error";
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

export interface AgentTask {
  task_id: string;
  agent: string;
  title: string;
  status: "planning" | "in_progress" | "done" | "error";
  created_at: string;
  updated_at: string;
  steps: TaskStep[];
}

// ── Session failures ──────────────────────────────────────────────────────────
// Source: crates/opex-core/src/gateway/handlers/session_failures.rs
// Backed by migration 034.

export type SessionFailureKind =
  | "llm_error"
  | "provider_error"
  | "tool_error"
  | "sub_agent_timeout"
  | "max_iterations"
  | "other"
  | string; // free-form fallback

export interface SessionFailureEntry {
  id: string;
  session_id: string;
  agent_id: string;
  failed_at: string;
  failure_kind: SessionFailureKind;
  error_message: string;
  last_tool_name: string | null;
  last_tool_output: string | null;
  llm_provider: string | null;
  llm_model: string | null;
  iteration_count: number | null;
  duration_secs: number | null;
  context: Record<string, unknown> | null;
}

export interface SessionFailuresResponse {
  failures: SessionFailureEntry[];
  total: number;
  limit: number;
  offset: number;
}

/**
 * `[agent_tool]` section of `AppConfig` — multi-agent timeouts.
 *
 * Field names mirror the Rust `AgentToolConfig` struct in
 * `crates/opex-core/src/config/mod.rs` exactly.
 */
export interface AgentToolConfig {
  message_wait_for_idle_secs: number;
  message_result_secs: number;
  safety_timeout_secs: number;
}

// ── Session compression chains ────────────────────────────────────────────────

export interface SessionChainEntry {
  id: string;
  parent_session_id: string | null;
  end_reason: string | null;
  title: string | null;
  started_at: string;
  agent_id: string;
  depth: number;
}

export interface SessionChainResponse {
  chain: SessionChainEntry[];
}

// ── File Handler Hub (Phase 4) ─────────────────────────────────────────────────
// Source: crates/opex-core/src/gateway/handlers/files.rs (HandlerButton)
// GET /api/files/{upload_id}/actions → { buttons: FileActionButton[] }
// `upload_id` is the upload ROW UUID (the `filename` field of POST /api/media/upload).
// `label` is already localized server-side by the request locale.

export interface FileActionButton {
  id: string;
  label: string;
  icon: string;
  params: Record<string, unknown>;
}

export interface FileActionsResponse {
  buttons: FileActionButton[];
}

// ── File Handlers admin (Tools tab) ────────────────────────────────────────────
// Source: crates/opex-core/src/gateway/handlers/handlers_admin.rs
// GET /api/handlers → { handlers: HandlerAdminRow[] }

export interface HandlerAdminRow {
  id: string;
  labels: Record<string, string>;        // { ru, en }
  descriptions: Record<string, string>;  // { ru, en }
  icon: string;
  match: { mime: string[]; max_size_mb: number | null }; // backend always emits mime ([] if none); max_size_mb null when uncapped
  capability?: string | null;
  provider?: string | null;
  execution: "sync" | "async";
  output: string;
  order: number;
  tier: "builtin" | "workspace";
  enabled: boolean;
  source: "builtin" | "override" | "workspace";
}

export interface HandlerSourceDto {
  id: string;
  source: string;
  source_kind: "builtin" | "override" | "workspace";
}

// ── Command registry (slash-command autocomplete) ──────────────────────────────
// Source: crates/opex-core/src/agent/commands/spec.rs (CommandSpec / CommandArg)
// GET /api/commands → { commands: CommandInfo[], version: string }
// Hand-mirrored (no ts-rs codegen for this gateway DTO yet). Fields beyond
// name/description/category/aliases/args are optional here because the web
// composer (Phase 1) only needs the core fields to render the dropdown.

export interface CommandChoice {
  value: string;
  label: string;
}

export type CommandChoices =
  | { kind: "static"; values: CommandChoice[] }
  | { kind: "dynamic"; provider: string };

export interface CommandArgInfo {
  name: string;
  description?: string;
  arg_type?: "string" | "number" | "boolean";
  required?: boolean;
  choices?: CommandChoices;
  capture_remaining?: boolean;
  menu?: boolean;
}

export type CommandSource =
  | { kind: "builtin" }
  | { kind: "handler"; handler_id: string };

export interface CommandInfo {
  name: string;
  description: string;
  category: string;
  aliases: string[];
  args: CommandArgInfo[];
  scope?: "text" | "native" | "both";
  visibility?: "all" | "base_only";
  source?: CommandSource;
}

export interface CommandListResponse {
  commands: CommandInfo[];
  version: string;
}

/** One entry from GET /api/handlers/allowlist — the 5 FSE_DEFAULT_ALLOWLIST members. */
export interface HandlerAllowlistRow {
  action_ref: string;
  enabled: boolean;
}

// ── Search palette (Ctrl+K) ──────────────────────────────────────────────────
// GET /api/sessions/search?q=&agent=|all=true&limit= — see
// crates/opex-core/src/gateway/handlers/sessions.rs::api_search_sessions

/** A message-level FTS hit. `snippet` carries `<b>`/`</b>` markers around the
 *  matched terms — render by splitting on the markers, NEVER dangerouslySetInnerHTML. */
export interface SearchMessageHit {
  message_id: string;
  content: string;
  session_id: string;
  session_title: string | null;
  agent_id: string;
  user_id: string | null;
  channel: string | null;
  role: string;
  created_at: string;
  rank: number;
  snippet: string;
}

/** A session-title FTS hit. */
export interface SearchSessionHit {
  session_id: string;
  title: string | null;
  agent_id: string;
  last_message_at: string;
}

export interface SearchResponse {
  messages: SearchMessageHit[];
  sessions: SearchSessionHit[];
  count: number;
}
