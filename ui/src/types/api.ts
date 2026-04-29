

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
// Source: crates/hydeclaw-core/src/gateway/handlers/agents/dto_structs.rs
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
// Source: crates/hydeclaw-core/src/gateway/handlers/agents/dto.rs
// Regenerate: make gen-types
export type { AgentDetailDto as AgentDetail } from "./api.generated";

// SessionRow is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/db/sessions.rs
// Regenerate: make gen-types
export type { Session as SessionRow } from "./api.generated";

// MessageRow is now generated from Rust DTO via ts-rs codegen.
// Note: feedback is number | null (DB-accurate); the old type had number (incorrect).
// Source: crates/hydeclaw-core/src/db/sessions.rs
// Regenerate: make gen-types
export type { MessageRow } from "./api.generated";

// CronJob is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/gateway/handlers/cron_dto_structs.rs
// Regenerate: make gen-types
export type { CronJobDto as CronJob } from "./api.generated";

// CronRun is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/gateway/handlers/cron_dto_structs.rs
// Regenerate: make gen-types
export type { CronRunDto as CronRun } from "./api.generated";

// MemoryDocument is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/gateway/handlers/memory_dto_structs.rs
// Regenerate: make gen-types
export type { MemoryDocumentDto as MemoryDocument } from "./api.generated";

// MemoryStats is now generated from Rust DTO via ts-rs codegen.
// Drift fix: tasks field (pending/processing/done/failed) was emitted by handler but absent from TS type.
// Source: crates/hydeclaw-core/src/gateway/handlers/memory_dto_structs.rs
// Regenerate: make gen-types
export type { MemoryStatsDto as MemoryStats } from "./api.generated";

// ToolEntry is now generated from Rust DTO via ts-rs codegen.
// Drift fixes: concurrency_limit is number (not number | null); managed is boolean (not optional).
// Source: crates/hydeclaw-core/src/gateway/handlers/tools_dto_structs.rs
// Regenerate: make gen-types
export type { ToolEntryDto as ToolEntry } from "./api.generated";

export interface SkillEntry {
  name: string;
  description: string;
  triggers: string[];
  tools_required: string[];
  priority: number;
  instructions_len: number;
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
// Source: crates/hydeclaw-core/src/gateway/handlers/tools_dto_structs.rs
// Regenerate: make gen-types
export type { McpEntryDto as McpEntry } from "./api.generated";

export interface FileEntry {
  name: string;
  is_dir: boolean;
  display: string;
}

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
// Source: crates/hydeclaw-core/src/gateway/handlers/channels_dto_structs.rs
// Regenerate: make gen-types
export type { ChannelRowDto as ChannelRow } from "./api.generated";

// ActiveChannel is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/gateway/handlers/channels_dto_structs.rs
// Regenerate: make gen-types
export type { ActiveChannelDto as ActiveChannel } from "./api.generated";

// BackupEntry is now generated from Rust DTO via ts-rs codegen.
// Drift fix: created_at is string | null (filesystem mtime can be absent).
// Source: crates/hydeclaw-core/src/gateway/handlers/backup_dto_structs.rs
// Regenerate: make gen-types
export type { BackupEntryDto as BackupEntry } from "./api.generated";

// WebhookEntry is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/gateway/handlers/webhooks_dto_structs.rs
// Regenerate: make gen-types
export type { WebhookEntryDto as WebhookEntry } from "./api.generated";

// ApprovalEntry is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/gateway/handlers/agents/approvals_dto_structs.rs
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
}

export interface MediaDriverInfo {
  driver: string;
  label: string;
  requires_key: boolean;
}

// NotificationRow is now generated from Rust DTO via ts-rs codegen.
// Source: crates/hydeclaw-core/src/db/notifications.rs
// Regenerate: make gen-types
export type { Notification as NotificationRow } from "./api.generated";

// NotificationsResponse is now generated. `items` is required (not optional);
// the stale `notifications?` key has been removed.
// Regenerate: make gen-types
export type { NotificationsResponseDto as NotificationsResponse } from "./api.generated";

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
// Source: crates/hydeclaw-core/src/gateway/handlers/session_failures.rs
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
 * `crates/hydeclaw-core/src/config/mod.rs` exactly.
 */
export interface AgentToolConfig {
  message_wait_for_idle_secs: number;
  message_result_secs: number;
  safety_timeout_secs: number;
}
