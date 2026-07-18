// @generated — do not edit by hand.
// Source of truth: types annotated with #[ts(export)] in crates/opex-core/.
// Regenerate with: make gen-types

export type ActiveChannelDto = { agent_name: string, channel_id: string | null, channel_type: string, display_name: string, adapter_version: string, connected_at: string, last_activity: string, };

export type AgentCapabilitiesDto = { text: boolean, stt: boolean, tts: boolean, vision: boolean, imagegen: boolean, websearch: boolean, };

export type AgentDetailAccessDto = { mode: string, owner_id: string | null, };

export type AgentDetailApprovalDto = { enabled: boolean, require_for: Array<string>, require_for_categories: Array<string>, timeout_seconds: number, };

export type AgentDetailCompactionDto = { enabled: boolean, threshold: number, preserve_tool_calls: boolean, preserve_last_n: number, max_context_tokens: number | null, };

export type AgentDetailDriftDto = { enabled: boolean, threshold: number, min_history: number, baseline_turns: number, z_fire: number, z_release: number, correct: boolean, anchor?: string, };

export type AgentDetailDto = { name: string, language: string, 
/**
 * Name of the row in the `profiles` table this agent resolves providers
 * from (replaces the removed provider/model/provider_connection/
 * fallback_provider/tts_provider/imagegen_provider fields).
 */
profile: string, capabilities: AgentCapabilitiesDto, temperature: number, max_tokens: number | null, access: AgentDetailAccessDto | null, heartbeat: AgentDetailHeartbeatDto | null, tools: AgentDetailToolsDto | null, compaction: AgentDetailCompactionDto | null, skill_review: AgentDetailSkillReviewDto | null, session: AgentDetailSessionDto | null, 
/**
 * Pre-signed URL for the icon under `/api/uploads/{id}`. Long-TTL
 * (`HISTORICAL_URL_TTL_SECS`) so a saved agent icon stays viewable across
 * restarts. `None` when the agent has no icon in the `uploads` table or
 * no upload key is available.
 */
icon_url: string | null, max_tools_in_context: number | null, tool_loop: AgentDetailToolLoopDto | null, tool_dispatcher: AgentDetailToolDispatcherDto | null, soul: AgentDetailSoulDto, drift: AgentDetailDriftDto, initiative: AgentDetailInitiativeDto, emotion: AgentDetailEmotionDto, approval: AgentDetailApprovalDto | null, routing: Array<AgentDetailRoutingDto>, watchdog: AgentDetailWatchdogDto | null, hooks: AgentDetailHooksDto | null, max_history_messages: number | null, daily_budget_tokens: number, max_failover_attempts: number, is_running: boolean, config_dirty: boolean, 
/**
 * Injected by the handler from scoped TTS_VOICE secret; absent when not set.
 */
voice?: string, };

export type AgentDetailEmotionDto = { enabled: boolean, intensity_importance_k: number, blend_rate: number, decay_half_life_hours: number, };

export type AgentDetailHeartbeatDto = { cron: string, timezone: string | null, announce_to: string | null, };

export type AgentDetailHooksDto = { log_all_tool_calls: boolean, block_tools: Array<string>, webhooks: Array<WebhookDto>, };

export type AgentDetailInitiativeDto = { enabled: boolean, daily_proposal_cap: number, decompose: boolean, daily_plan: boolean, auto_approve_day_plan: boolean, daily_token_budget: number, };

export type AgentDetailRoutingDto = { condition: string, connection: string | null, model: string | null, temperature: number | null, cooldown_secs: number, };

export type AgentDetailSessionDto = { dm_scope: string, ttl_days: number, max_messages: number, prune_tool_output_after_turns: number | null, };

export type AgentDetailSkillReviewDto = { enabled: boolean, min_tool_calls: number, };

export type AgentDetailSoulDto = { enabled: boolean, reflection_threshold: number, reflection_cooldown_minutes: number, context_top_k: number, context_budget_tokens: number, max_events_per_session: number, };

export type AgentDetailToolDispatcherDto = { enabled: boolean, core_extra: Array<string>, promotion_max: number, };

export type AgentDetailToolGroupsDto = { git: boolean, tool_management: boolean, skill_editing: boolean, session_tools: boolean, };

export type AgentDetailToolLoopDto = { max_iterations: number, compact_on_overflow: boolean, detect_loops: boolean, warn_threshold: number, break_threshold: number, max_consecutive_failures: number, max_auto_continues: number, max_loop_nudges: number, ngram_cycle_length: number, };

export type AgentDetailToolsDto = { allow: Array<string>, deny: Array<string>, allow_all: boolean, deny_all_others: boolean, groups: AgentDetailToolGroupsDto, };

export type AgentDetailWatchdogDto = { inactivity_secs: number, };

export type AgentInfoDto = { name: string, language: string, 
/**
 * See `AgentDetailDto::profile`.
 */
profile: string, capabilities: AgentCapabilitiesDto, 
/**
 * Pre-signed URL for the icon (see `AgentDetailDto::icon_url`).
 */
icon_url: string | null, temperature: number, has_access: boolean, access_mode: string | null, has_heartbeat: boolean, heartbeat_cron: string | null, heartbeat_timezone: string | null, tool_policy: AgentInfoToolPolicyDto | null, routing_count: number, is_running: boolean, config_dirty: boolean, base?: boolean, pending_delete?: boolean, };

export type AgentInfoToolPolicyDto = { allow: Array<string>, deny: Array<string>, allow_all: boolean, };

export type AllowlistEntry = { id: string, agent_id: string, tool_pattern: string, created_at: string, created_by: string | null, };

export type ApprovalEntryDto = { id: string, agent_id: string, tool: string, arguments: Record<string, unknown>, status: "pending" | "approved" | "rejected", created_at: string, resolved_at: string | null, resolved_by: string | null, };

export type BackupEntryDto = { filename: string, size_bytes: number, created_at: string | null, };

export type ChannelRowDto = { id: string, agent_name: string, channel_type: string, display_name: string, config: Record<string, unknown>, status: string, error_msg: string | null, };

export type CheckpointListDto = { enabled: boolean, items: Array<CheckpointMetaDto>, };

export type CheckpointMetaDto = { n: number, commit: string, created: string, summary: string, };

export type CronJobDto = { id: string, name: string, agent: string, cron: string, timezone: string, task: string, enabled: boolean, silent: boolean, announce_to?: { channel: string; chat_id: number; channel_id?: string }, jitter_secs: number, run_once: boolean, run_at: string | null, created_at: string, last_run: string | null, next_run: string | null, tool_policy?: { allow: string[]; deny: string[] }, };

export type CronRunDto = { id: string, job_id: string, job_name?: string, agent_id: string, started_at: string, finished_at: string | null, status: "running" | "success" | "error", error: string | null, response_preview: string | null, };

export type GitHubRepo = { id: string, agent_id: string, owner: string, repo: string, added_at: string, };

export type McpEntryDto = { name: string, url: string | null, container: string | null, port: number | null, mode: string, protocol: string, enabled: boolean, status: string | null, tool_count: number | null, };

export type MemoryDocumentDto = { id: string, source: string | null, pinned: boolean, relevance_score: number, similarity?: number, created_at?: string, accessed_at?: string, preview: string | null, total_chars: number | null, scope?: string, 
/**
 * 'fact' | 'event' | 'reflection' (soul foundation, m076)
 */
kind: string, 
/**
 * LLM importance 1-10 (soul retrieval scoring); 5.0 for legacy rows
 */
importance: number, };

export type MemoryStatsDto = { total: number, total_chunks: number, pinned: number, avg_score: number, embed_model?: string, embed_dim?: number, tasks: MemoryTaskStatsDto, };

export type MemoryTaskStatsDto = { pending: number, processing: number, done: number, failed: number, };

export type MessageRow = { id: string, role: string, content: string, tool_calls: unknown, tool_call_id: string | null, created_at: string, agent_id: string | null, feedback: number | null, edited_at: string | null, status: string, thinking_blocks: unknown, parent_message_id: string | null, branch_from_message_id: string | null, abort_reason: string | null, is_mirror: boolean, bookmarked_at: string | null, };

export type Notification = { id: string, type: string, title: string, body: string, data: Record<string, unknown>, read: boolean, created_at: string, };

export type NotificationsResponseDto = { items: Array<Notification>, unread_count: number, limit: number, offset: number, };

export type RestoreReportDto = { n: number, files: Array<string>, new_checkpoint: number | null, };

export type Session = { id: string, agent_id: string, user_id: string, channel: string, 
/**
 * Per-chat/group/thread disambiguator (see `dm_scope_keys` doc). `None`
 * for pre-migration rows and platforms with no chat concept.
 */
chat_scope: string | null, started_at: string, last_message_at: string, title: string | null, metadata: Record<string, unknown> | null, run_status: string | null, participants: Array<string>, parent_session_id: string | null, end_reason: string | null, };

export type ToolEntryDto = { name: string, url: string, tool_type: string, concurrency_limit: number, healthy: boolean, healthcheck?: string, depends_on: Array<string>, ui_path?: string, managed: boolean, };

export type WebhookDto = { url: string, events: Array<string>, mode: string, tool_matcher: string | null, on_failure: string, timeout_ms: number, allow_internal: boolean, };

export type WebhookEntryDto = { id: string, name: string, agent_id: string, secret: string | null, prompt_prefix: string | null, enabled: boolean, created_at: string, last_triggered_at: string | null, trigger_count: number, webhook_type: "generic" | "github", event_filter: Array<string> | null, };
