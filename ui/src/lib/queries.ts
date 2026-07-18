import { useQuery, useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { toast } from "sonner"
import { apiGet, apiPost, apiPut, apiDelete, apiPatch, listCheckpoints, restoreCheckpoint, getAgentPlan, approveProposal, dismissProposal, cancelGoal } from "./api"
import { useNotificationStore } from "@/stores/notification-store"
import { useWsStore } from "@/stores/ws-store"
import { useWsSubscription } from "@/hooks/use-ws-subscription"
import type { NotificationsResponse, SessionFailuresResponse, SessionChainResponse } from "@/types/api"
import type { CheckpointListDto, RestoreReportDto } from "@/types/api.generated"
import type {
  AgentInfo,
  SecretInfo,
  CronJob,
  CronRun,
  MemoryStats,
  ToolEntry,
  YamlToolEntry,
  McpEntry,
  ChannelRow,
  ActiveChannel,
  AuditEvent,
  UsageResponse,
  DailyUsageResponse,
  SkillEntry,
  WebhookEntry,
  ApprovalEntry,
  BackupEntry,
  SessionRow,
  MessageRow,
  Provider,
  ProviderType,
  CreateProviderInput,
  ProviderActiveRow,
  MediaDriverInfo,
  OAuthAccount,
  OAuthBinding,
  AgentTask,
  CuratorStatus,
  CuratorConfig,
  CuratorRun,
  SkillVersion,
  CuratorDecision,
  SkillCuratorDecisions,
  HandlerAdminRow,
  HandlerAllowlistRow,
  HandlerSourceDto,
  AgentPlan,
} from "@/types/api"

// ── Query Keys ──────────────────────────────────────────────────────────────

export const qk = {
  agents: ["agents"] as const,
  agent: (name: string) => ["agents", name] as const,
  agentChannels: (name: string) => ["agents", name, "channels"] as const,
  tools: ["tools"] as const,
  yamlTools: ["yaml-tools"] as const,
  mcpServers: ["mcp"] as const,
  secrets: ["secrets"] as const,
  skills: ["skills"] as const,
  channels: ["channels"] as const,
  activeChannels: ["channels", "active"] as const,
  cron: ["cron"] as const,
  cronRuns: (jobId: string) => ["cron", jobId, "runs"] as const,
  cronRunsAll: ["cron", "runs"] as const,
  memoryStats: ["memory", "stats"] as const,
  audit: (params: Record<string, string>) => ["audit", params] as const,
  config: ["config"] as const,
  access: ["access"] as const,
  usage: (days: number) => ["usage", days] as const,
  dailyUsage: (days: number) => ["usage", "daily", days] as const,
  providerModels: (id: string) => ["providers", id, "models"] as const,
  ttsVoices: (provider: string) => ["tts-voices", provider] as const,
  webhooks: ["webhooks"] as const,
  approvals: ["approvals"] as const,
  backups: ["backups"] as const,
  sessions: (agent: string) => ["sessions", "list", agent] as const,
  sessionMessages: (id: string) => ["sessions", id, "messages"] as const,
  sessionChain: (id: string) => ["sessions", id, "chain"] as const,
  providers: ["providers"] as const,
  providerTypes: ["provider-types"] as const,
  providerActive: ["provider-active"] as const,
  mediaDrivers: ["media-drivers"] as const,
  oauthAccounts: ["oauth", "accounts"] as const,
  oauthBindings: (agent: string) => ["oauth", "bindings", agent] as const,
  notificationPrefs: ["notification-prefs"] as const,
  notifications: ["notifications"] as const,
  agentTasks: (name: string) => ["agents", name, "tasks"] as const,
  sessionFailures: (agent: string | null, limit: number) =>
    ["session-failures", agent, limit] as const,
  curatorStatus: ["curator", "status"] as const,
  curatorRuns: ["curator", "runs"] as const,
  curatorDecisions: ["curator-decisions"] as const,
  skillCuratorDecisions: (name: string) => ["skills", name, "curator-decisions"] as const,
  checkpoints: (name: string) => ["agents", name, "checkpoints"] as const,
  agentPlan: (name: string) => ["agents", name, "plan"] as const,
  handlers: ["handlers"] as const,
  handlerAllowlist: ["handlers", "allowlist"] as const,
}

// ── Query Hooks ─────────────────────────────────────────────────────────────

export function useAgents() {
  return useQuery({
    queryKey: qk.agents,
    queryFn: () => apiGet<{ agents: AgentInfo[] }>("/api/agents"),
    select: (d) => d.agents,
  })
}

export function useSecrets() {
  return useQuery({
    queryKey: qk.secrets,
    queryFn: () => apiGet<{ secrets: SecretInfo[] }>("/api/secrets"),
    select: (d) => d.secrets,
  })
}

export function useTools() {
  return useQuery({
    queryKey: qk.tools,
    queryFn: () => apiGet<{ tools: ToolEntry[] }>("/api/tools"),
    select: (d) => d.tools,
  })
}

export function useYamlTools() {
  return useQuery({
    queryKey: qk.yamlTools,
    queryFn: () => apiGet<{ tools: YamlToolEntry[] }>("/api/yaml-tools"),
    select: (d) => d.tools,
  })
}

export function useMcpServers() {
  return useQuery({
    queryKey: qk.mcpServers,
    queryFn: () => apiGet<{ mcp: McpEntry[] }>("/api/mcp"),
    select: (d) => d.mcp,
  })
}

export function useSkills() {
  return useQuery({
    queryKey: qk.skills,
    queryFn: () => apiGet<{ skills: SkillEntry[] }>("/api/skills"),
    select: (d) => d.skills,
  })
}

export function useCronJobs() {
  return useQuery({
    queryKey: qk.cron,
    queryFn: () => apiGet<{ jobs: CronJob[] }>("/api/cron"),
    select: (d) => d.jobs,
  })
}

export function useCronRuns(jobId: string | null) {
  return useQuery({
    queryKey: qk.cronRuns(jobId ?? ""),
    queryFn: () => apiGet<{ runs: CronRun[] }>(`/api/cron/${jobId}/runs`),
    select: (d) => d.runs,
    enabled: !!jobId,
  })
}

export function useChannels() {
  return useQuery({
    queryKey: qk.channels,
    queryFn: () => apiGet<{ channels: ChannelRow[] }>("/api/channels"),
    select: (d) => d.channels,
  })
}

export function useActiveChannels() {
  return useQuery({
    queryKey: qk.activeChannels,
    queryFn: () => apiGet<{ channels: ActiveChannel[] }>("/api/channels/active"),
    select: (d) => d.channels,
  })
}

export function useMemoryStats() {
  return useQuery({
    queryKey: qk.memoryStats,
    queryFn: () => apiGet<MemoryStats>("/api/memory/stats"),
  })
}

export function useAudit(params: Record<string, string> = {}) {
  const qs = new URLSearchParams(params).toString()
  return useQuery({
    queryKey: qk.audit(params),
    queryFn: () => apiGet<{ events: AuditEvent[] }>(`/api/audit${qs ? `?${qs}` : ""}`),
    select: (d) => d.events,
  })
}

export function useUsage(days = 30) {
  return useQuery({
    queryKey: qk.usage(days),
    queryFn: () => apiGet<UsageResponse>(`/api/usage?days=${days}`),
  })
}

export function useDailyUsage(days = 30) {
  return useQuery({
    queryKey: qk.dailyUsage(days),
    queryFn: () => apiGet<DailyUsageResponse>(`/api/usage/daily?days=${days}`),
  })
}

export function useProviderModels(id: string | null) {
  return useQuery({
    queryKey: qk.providerModels(id ?? ""),
    queryFn: () => apiGet<{ models: Array<string | { id: string }> }>(`/api/providers/${id}/models`),
    select: (d) => d.models.map((m) => typeof m === "string" ? m : m.id),
    enabled: !!id,
    retry: false,
    staleTime: 60_000,
  })
}

/** A discovered model enriched with catalog metadata (context window + caps). */
export interface ProviderModel {
  id: string
  owned_by?: string
  context_window?: number
  vision?: boolean
  reasoning?: boolean
  /** Uses a `reasoning_content` field (DeepSeek-R1, Kimi-thinking, …). */
  reasoning_content?: boolean
  tools?: boolean
}

/** Same endpoint/queryKey as {@link useProviderModels} (shared fetch), but keeps
 *  the full per-model objects so selectors can show context-window + capability
 *  badges. */
export function useProviderModelsDetailed(id: string | null) {
  return useQuery({
    queryKey: qk.providerModels(id ?? ""),
    queryFn: () => apiGet<{ models: Array<string | ProviderModel> }>(`/api/providers/${id}/models`),
    select: (d): ProviderModel[] => d.models.map((m) => (typeof m === "string" ? { id: m } : m)),
    enabled: !!id,
    retry: false,
    staleTime: 60_000,
  })
}

export interface TtsVoice {
  id: string
  name: string
  description?: string
  language?: string
}

/** Voice list of a TTS provider (GET /api/tts/voices?provider=). Feeds the
 *  shared VoiceSelect field. */
export function useTtsVoices(provider: string | null) {
  return useQuery({
    queryKey: qk.ttsVoices(provider ?? ""),
    queryFn: () => apiGet<{ voices: TtsVoice[] }>(`/api/tts/voices?provider=${encodeURIComponent(provider ?? "")}`),
    select: (d) => d.voices ?? [],
    enabled: !!provider,
    retry: false,
    staleTime: 60_000,
  })
}

export function useApprovals() {
  return useQuery({
    queryKey: qk.approvals,
    queryFn: () => apiGet<{ approvals: ApprovalEntry[] }>("/api/approvals"),
    select: (d) => d.approvals ?? [],
    refetchInterval: 5000,
  })
}

export function useSessionFailures(agent: string | null, limit = 20) {
  const params = new URLSearchParams()
  if (agent) params.set("agent", agent)
  params.set("limit", String(limit))
  return useQuery({
    queryKey: qk.sessionFailures(agent, limit),
    queryFn: () =>
      apiGet<SessionFailuresResponse>(`/api/sessions/failures?${params.toString()}`),
    // The backend requires a specific ?agent= (it refuses a bulk cross-agent
    // list with 400); don't fire until one is chosen.
    enabled: !!agent,
    refetchInterval: 30_000,
  })
}

export function useAgentTasks(agentName: string | null, isStreaming = false) {
  return useQuery({
    queryKey: qk.agentTasks(agentName!),
    queryFn: () => apiGet<{ tasks: AgentTask[] }>(`/api/agents/${agentName}/tasks`),
    select: (d) => d.tasks,
    enabled: !!agentName,
    // Most agents never use task plans — keep polling cheap by stopping
    // entirely when idle AND no tasks have been seen. Streaming resumes
    // polling so new tasks created mid-turn show up quickly.
    refetchInterval: (query) => {
      const taskCount = query.state.data?.tasks?.length ?? 0;
      if (!isStreaming && taskCount === 0) return false;
      return isStreaming ? 3000 : 15000;
    },
    staleTime: 2500,
  })
}

// ── Curator Hooks ────────────────────────────────────────────────────────────

export function useCuratorStatus() {
  return useQuery<CuratorStatus>({
    queryKey: qk.curatorStatus,
    queryFn: () => apiGet<CuratorStatus>("/api/curator/status"),
  })
}

export function useCuratorRuns() {
  return useQuery<{ runs: CuratorRun[] }>({
    queryKey: qk.curatorRuns,
    queryFn: () => apiGet<{ runs: CuratorRun[] }>("/api/curator/runs"),
  })
}

export function useSkillVersions(skillName: string) {
  return useQuery<{ versions: SkillVersion[] }>({
    queryKey: [...qk.skills, skillName, "versions"],
    queryFn: () => apiGet<{ versions: SkillVersion[] }>(`/api/skills/${encodeURIComponent(skillName)}/versions`),
    enabled: !!skillName,
  })
}

export function useCuratorConfig() {
  return useQuery({
    queryKey: ["curator", "config"] as const,
    queryFn: () => apiGet<CuratorConfig>("/api/curator/config"),
  })
}

export function useCuratorDecisions() {
  return useQuery({
    queryKey: qk.curatorDecisions,
    queryFn: () => apiGet<Record<string, CuratorDecision>>("/api/curator-decisions/recent"),
    staleTime: 60_000,
  })
}

export function useSkillCuratorDecisions(skillName: string) {
  return useQuery({
    queryKey: qk.skillCuratorDecisions(skillName),
    queryFn: () => apiGet<SkillCuratorDecisions>(
      `/api/skills/${encodeURIComponent(skillName)}/curator-decisions?limit=5`
    ),
    enabled: !!skillName,
  })
}

// ── Mutation Hooks ──────────────────────────────────────────────────────────

export function useUpsertSecret() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { name: string; value: string; description?: string; scope?: string }) =>
      apiPost("/api/secrets", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.secrets }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useDeleteSecret() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ name, scope }: { name: string; scope?: string }) => {
      const scopeParam = scope ? `?scope=${encodeURIComponent(scope)}` : ""
      return apiDelete(`/api/secrets/${encodeURIComponent(name)}${scopeParam}`)
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.secrets }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useUpdateAgent() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (body: { name: string } & Record<string, unknown>) =>
      apiPut(`/api/agents/${body.name}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.agents }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useCreateCronJob() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: Record<string, unknown>) => apiPost("/api/cron", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.cron }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useUpdateCronJob() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, ...body }: { id: string } & Record<string, unknown>) =>
      apiPut(`/api/cron/${id}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.cron }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useDeleteCronJob() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => apiDelete(`/api/cron/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.cron }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useRunCronJob() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => apiPost(`/api/cron/${id}/run`),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: qk.cron })
      qc.invalidateQueries({ queryKey: qk.cronRunsAll })
    },
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useRestartService() {
  return useMutation({
    mutationFn: (name: string) => apiPost(`/api/services/${name}/restart`),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useRebuildService() {
  return useMutation({
    mutationFn: (name: string) => apiPost(`/api/services/${name}/rebuild`),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useResolveApproval() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, status }: { id: string; status: "approved" | "rejected" }) =>
      apiPost(`/api/approvals/${id}/resolve`, { status, resolved_by: "ui" }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.approvals }),
    onError: (e: Error) => toast.error(e.message),
  })
}

// ── Webhooks ────────────────────────────────────────────────────────────────

export function useWebhooks() {
  return useQuery({
    queryKey: qk.webhooks,
    queryFn: () => apiGet<{ webhooks: WebhookEntry[] }>("/api/webhooks"),
    select: (d) => d.webhooks,
  })
}

export function useCreateWebhook() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { name: string; agent: string; prompt_prefix?: string; webhook_type?: string; event_filter?: string[] }) =>
      apiPost<{ secret?: string }>("/api/webhooks", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.webhooks }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useUpdateWebhook() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, ...body }: { id: string; name?: string; agent?: string; prompt_prefix?: string; enabled?: boolean; webhook_type?: string; event_filter?: string[] }) =>
      apiPut(`/api/webhooks/${id}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.webhooks }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useDeleteWebhook() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => apiDelete(`/api/webhooks/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.webhooks }),
    onError: (e: Error) => toast.error(e.message),
  })
}

// ── Backups ──────────────────────────────────────────────────────────────────

export function useBackups() {
  return useQuery({
    queryKey: qk.backups,
    queryFn: () => apiGet<{ backups: BackupEntry[] }>("/api/backup"),
    select: (d) => d.backups ?? [],
  })
}

export function useCreateBackup() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: () => apiPost("/api/backup"),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.backups }),
    onError: (e: Error) => toast.error(e.message),
  })
}

// ── Chat Session Hooks (persisted to IDB via PersistQueryClientProvider) ────

/** Page size for the keyset-paginated session list — must match the backend
 *  default and the `hasNextPage` full-page check below. */
export const SESSIONS_PAGE_SIZE = 40

/** One page of the keyset-paginated `/api/sessions` response. */
export interface SessionsPage {
  sessions: SessionRow[]
  total: number
}

/** Keyset cursor for the next (older) page — `(last_message_at, id)` of the
 *  last row of the page just loaded. `null` = first page (no cursor params). */
export type SessionsCursor = {
  before_last_message_at: string
  before_id: string
} | null

/** Raw infinite-query cache shape for the session list. */
export interface SessionsInfiniteData {
  pages: SessionsPage[]
  pageParams: unknown[]
}

/** Flatten the infinite-query cache into the legacy flat `SessionRow[]` shape,
 *  deduping by id (keyset paging shouldn't repeat rows, but a create/refetch
 *  race across a page boundary can — dedup keeps the merged list clean).
 *  First occurrence wins, so newest-first order is preserved. Exported for the
 *  direct cache reader (stream-processor) that can't go through the hook. */
export function flatSessionsFromCache(
  data: { pages: SessionsPage[] } | undefined,
): SessionRow[] {
  if (!data?.pages) return []
  const seen = new Set<string>()
  const out: SessionRow[] = []
  for (const page of data.pages) {
    for (const s of page.sessions) {
      if (seen.has(s.id)) continue
      seen.add(s.id)
      out.push(s)
    }
  }
  return out
}

/** getNextPageParam for the session list: a full page (=== PAGE_SIZE) yields the
 *  `(last_message_at, id)` cursor of its last row; a short page means the end
 *  (`undefined` → `hasNextPage=false`). */
export function sessionsGetNextPageParam(
  lastPage: SessionsPage,
): SessionsCursor | undefined {
  if (lastPage.sessions.length < SESSIONS_PAGE_SIZE) return undefined
  const last = lastPage.sessions[lastPage.sessions.length - 1]
  return { before_last_message_at: last.last_message_at, before_id: last.id }
}

/** Referentially-minimal title patch across the infinite cache — used by rename
 *  so the list updates WITHOUT a refetch that would reset the sidebar Virtuoso
 *  scroll position. Pages whose rows are untouched keep their identity. */
export function patchSessionTitleInPages(
  data: SessionsInfiniteData | undefined,
  sessionId: string,
  title: string,
): SessionsInfiniteData | undefined {
  if (!data?.pages) return data
  return {
    ...data,
    pages: data.pages.map((page) =>
      page.sessions.some((s) => s.id === sessionId)
        ? {
            ...page,
            sessions: page.sessions.map((s) =>
              s.id === sessionId ? { ...s, title } : s,
            ),
          }
        : page,
    ),
  }
}

export interface UseSessionsResult {
  /** Already-flat, deduped, newest-first merge of every loaded page. */
  sessions: SessionRow[]
  total: number
  isLoading: boolean
  isFetched: boolean
  fetchNextPage: () => void
  hasNextPage: boolean
  isFetchingNextPage: boolean
}

/**
 * Keyset-paginated session list. Wraps `useInfiniteQuery` but returns the
 * PREVIOUS flat shape (`sessions`/`total`) so consumers stay unchanged, plus
 * the infinite-scroll controls the sidebar needs.
 */
export function useSessions(agent: string): UseSessionsResult {
  const query = useInfiniteQuery({
    queryKey: qk.sessions(agent),
    queryFn: ({ pageParam }) => {
      const params = new URLSearchParams({
        limit: String(SESSIONS_PAGE_SIZE),
        agent,
      })
      if (pageParam) {
        params.set("before_last_message_at", pageParam.before_last_message_at)
        params.set("before_id", pageParam.before_id)
      }
      return apiGet<SessionsPage>(`/api/sessions?${params.toString()}`)
    },
    initialPageParam: null as SessionsCursor,
    getNextPageParam: sessionsGetNextPageParam,
    enabled: !!agent,
    staleTime: 0, // Always refetch on mount for fresh data
    // No polling needed — session status is server-driven via WS agent_processing events
  })

  // Stable identity tied to query.data so consumers' effect deps don't churn.
  const sessions = useMemo(() => flatSessionsFromCache(query.data), [query.data])
  const total = query.data?.pages[0]?.total ?? sessions.length

  return {
    sessions,
    total,
    isLoading: query.isLoading,
    isFetched: query.isFetched,
    fetchNextPage: query.fetchNextPage,
    hasNextPage: query.hasNextPage,
    isFetchingNextPage: query.isFetchingNextPage,
  }
}

/** Minimum filtered rows we try to keep visible while a client-side filter is
 *  active — see {@link useAutoPaginateWhileFiltering}. */
export const SESSIONS_FILTER_MIN_VISIBLE = 20

/**
 * Keep pulling older pages while a client-side filter is active but hasn't yet
 * surfaced enough visible rows. Virtuoso's `endReached` never fires on a short
 * filtered list, so without this a filter could never reach sessions living
 * beyond the currently-loaded window. No-op when no filter is active.
 */
export function useAutoPaginateWhileFiltering(opts: {
  filterActive: boolean
  visibleCount: number
  hasNextPage: boolean
  isFetchingNextPage: boolean
  fetchNextPage: () => void
}) {
  const { filterActive, visibleCount, hasNextPage, isFetchingNextPage, fetchNextPage } = opts
  useEffect(() => {
    if (
      filterActive &&
      hasNextPage &&
      !isFetchingNextPage &&
      visibleCount < SESSIONS_FILTER_MIN_VISIBLE
    ) {
      fetchNextPage()
    }
  }, [filterActive, visibleCount, hasNextPage, isFetchingNextPage, fetchNextPage])
}

export function useProviders() {
  return useQuery({
    queryKey: qk.providers,
    queryFn: () => apiGet<{ providers: Provider[] }>("/api/providers"),
    select: (d) => d.providers,
    staleTime: 30_000,
  })
}

export function useProviderActive() {
  return useQuery({
    queryKey: qk.providerActive,
    queryFn: () => apiGet<{ active: ProviderActiveRow[] }>("/api/provider-active"),
    select: (d) => d.active,
    staleTime: 30_000,
  })
}

export function useProviderTypes() {
  return useQuery({
    queryKey: qk.providerTypes,
    queryFn: () => apiGet<{ provider_types: ProviderType[] }>("/api/provider-types"),
    select: (d) => d.provider_types,
    staleTime: Infinity,
  })
}

export function useCreateProvider() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: CreateProviderInput) => apiPost("/api/providers", data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: qk.providers })
      qc.invalidateQueries({ queryKey: qk.providerActive })
    },
    // No generic onError here: the only caller (providers page `save()`)
    // awaits mutateAsync in a try/catch and shows its own
    // "providers.save_error" toast — a hook-level toast here would double-fire
    // alongside it (TanStack calls both).
  })
}

export function useUpdateProvider() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, ...body }: { id: string } & Partial<CreateProviderInput>) =>
      apiPut(`/api/providers/${id}`, body),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: qk.providers })
      qc.invalidateQueries({ queryKey: qk.providerActive })
    },
    // No generic onError here: the only caller (providers page `save()`)
    // awaits mutateAsync in a try/catch and shows its own
    // "providers.save_error" toast — a hook-level toast here would double-fire
    // alongside it (TanStack calls both).
  })
}

export function useDeleteProvider() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => apiDelete(`/api/providers/${id}`),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: qk.providers })
      qc.invalidateQueries({ queryKey: qk.providerActive })
    },
    // No generic onError here: the only caller (providers page) supplies its
    // own onError per-mutate() call to distinguish the 409 "used by profiles"
    // case from other failures — a hook-level toast here would double-fire
    // alongside it (TanStack calls both).
  })
}

export function useSetProviderActive() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { capability: string; providers: { provider_name: string; priority: number }[] }) =>
      apiPut("/api/provider-active", data),
    onMutate: async (vars) => {
      await qc.cancelQueries({ queryKey: qk.providerActive })
      const snapshot = qc.getQueryData<{ active: ProviderActiveRow[] }>(qk.providerActive)
      qc.setQueryData<{ active: ProviderActiveRow[] }>(qk.providerActive, (old) => {
        const base = old?.active ?? []
        const others = base.filter((r) => r.capability !== vars.capability)
        const next: ProviderActiveRow[] = vars.providers.map((p) => ({
          capability: vars.capability,
          provider_name: p.provider_name,
          priority: p.priority,
        }))
        return { active: [...others, ...next] }
      })
      return { snapshot }
    },
    onError: (_e: Error, _vars, ctx) => {
      if (ctx?.snapshot) qc.setQueryData(qk.providerActive, ctx.snapshot)
    },
    onSettled: () => qc.invalidateQueries({ queryKey: qk.providerActive }),
  })
}

export function useSessionMessages(sessionId: string | null, agent?: string) {
  return useQuery({
    // 4-element key: getCachedHistoryMessages / getCachedRawMessages use
    // getQueriesData with the 3-element prefix so they find this entry
    // regardless of the agent suffix. Agent suffix prevents cross-agent
    // cache collisions and matches the subscription in useRenderMessages.
    queryKey: [...qk.sessionMessages(sessionId!), agent ?? ""] as const,
    queryFn: () => {
      // Audit 2026-05-08: backend requires ?agent= for owner check.
      const params = new URLSearchParams({ limit: "100" });
      if (agent) params.set("agent", agent);
      return apiGet<{ messages: MessageRow[] }>(
        `/api/sessions/${sessionId}/messages?${params.toString()}`,
      );
    },
    // Don't fire until agent is known — backend rejects requests without ?agent=.
    enabled: !!sessionId && !!agent,
    staleTime: 2000,
    gcTime: 24 * 60 * 60 * 1000,
    // Fallback poll for partial-buffer edge cases when a row is still
    // streaming. SSE delivers new messages directly otherwise.
    refetchInterval: (query) => {
      const msgs = (query.state.data as { messages: MessageRow[] } | undefined)?.messages
      return msgs?.some(m => m.status === "streaming") ? 3000 : false
    },
  })
}

export function useSessionChain(sessionId: string | null, agent?: string) {
  return useQuery({
    queryKey: [...qk.sessionChain(sessionId!), agent ?? ""] as const,
    queryFn: () => {
      // Audit 2026-05-08: backend requires ?agent= for owner check.
      const params = new URLSearchParams();
      if (agent) params.set("agent", agent);
      const qs = params.toString();
      return apiGet<SessionChainResponse>(
        `/api/sessions/${sessionId}/chain${qs ? `?${qs}` : ""}`,
      );
    },
    enabled: !!sessionId && !!agent,
    staleTime: 30_000,
  });
}

// ── Media Drivers ───────────────────────────────────────────────────────────

export function useMediaDrivers() {
  return useQuery({
    queryKey: qk.mediaDrivers,
    queryFn: () => apiGet<{ drivers: Record<string, MediaDriverInfo[]> }>("/api/media-drivers"),
    select: (d) => d.drivers,
    staleTime: Infinity,
  })
}

// ── OAuth Accounts & Bindings ───────────────────────────────────────────────

export function useOAuthAccounts() {
  return useQuery({
    queryKey: qk.oauthAccounts,
    queryFn: () => apiGet<{ accounts: OAuthAccount[] }>("/api/oauth/accounts"),
    select: (d) => d.accounts,
  })
}

export function useOAuthBindings(agent: string) {
  return useQuery({
    queryKey: qk.oauthBindings(agent),
    queryFn: () => apiGet<{ bindings: OAuthBinding[] }>(`/api/agents/${agent}/oauth/bindings`),
    select: (d) => d.bindings,
    enabled: !!agent,
  })
}

// ── Notifications ──────────────────────────────────────────────────────────

export function useNotifications() {
  const syncFirstPage = useNotificationStore((s) => s.syncFirstPage);
  const query = useQuery({
    queryKey: qk.notifications,
    queryFn: () => apiGet<NotificationsResponse>("/api/notifications?limit=20&offset=0"),
    refetchOnWindowFocus: true,
    refetchInterval: 60_000,
    refetchIntervalInBackground: false,
  });
  useEffect(() => {
    if (query.data) {
      syncFirstPage(query.data.items, query.data.unread_count);
    }
  }, [query.data, syncFirstPage]);
  return query;
}

/**
 * History pagination for the notification bell. Not a useQuery/useInfiniteQuery:
 * the live head of the list is owned by the store (WS prepends + first-page
 * merge), so we only ever fetch strictly-OLDER pages and append them. Uses the
 * `(created_at, id)` cursor of the oldest row currently in the store.
 */
export function useLoadOlderNotifications() {
  const appendOlder = useNotificationStore((s) => s.appendOlder);
  const [isLoading, setIsLoading] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const inFlightRef = useRef(false);

  const loadOlder = useCallback(async () => {
    if (inFlightRef.current || !hasMore) return;
    const list = useNotificationStore.getState().notifications;
    const oldest = list[list.length - 1];
    if (!oldest) return;
    inFlightRef.current = true;
    setIsLoading(true);
    try {
      const page = await apiGet<NotificationsResponse>(
        `/api/notifications?limit=20&before=${encodeURIComponent(oldest.created_at)}&before_id=${oldest.id}`,
      );
      appendOlder(page.items);
      if (page.items.length < 20) setHasMore(false);
    } catch {
      // transient network error — allow retry on the next scroll
    } finally {
      inFlightRef.current = false;
      setIsLoading(false);
    }
  }, [appendOlder, hasMore]);

  return { loadOlder, isLoading, hasMore };
}

export function useMarkNotificationRead() {
  const markRead = useNotificationStore((s) => s.markRead);
  return useMutation({
    mutationFn: (id: string) => apiPatch<unknown>(`/api/notifications/${id}`),
    onSuccess: (_data, id) => {
      markRead(id);
    },
  });
}

export function useMarkAllRead() {
  const markAllRead = useNotificationStore((s) => s.markAllRead);
  return useMutation({
    mutationFn: () => apiPost<unknown>("/api/notifications/read-all"),
    onSuccess: () => {
      markAllRead();
    },
  });
}

export function useClearAllNotifications() {
  const clearAll = useNotificationStore((s) => s.clearAll);
  return useMutation({
    mutationFn: () => apiDelete("/api/notifications/clear"),
    onSuccess: () => {
      clearAll();
    },
  });
}

// Owner resolves an `infra_decision` notification (self-healing infra —
// Task 5 backend) — "Выполнить"/"Отклонить" buttons rendered inline in the
// bell (see `NotificationInfraBody`). Invalidates the notifications query so
// the mounted `useNotifications()` refetch drops the resolved decision.
export function useResolveInfraDecision() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, approved }: { id: string; approved: boolean }) =>
      apiPost<unknown>(`/api/infra/decisions/${id}/resolve`, { approved }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: qk.notifications });
    },
  });
}

export interface NotificationPref {
  type: string;
  muted: boolean;
  sound: boolean;
}
interface NotificationPrefsResponse {
  prefs: NotificationPref[];
}

export function useNotificationPrefs() {
  const setPrefs = useNotificationStore((s) => s.setPrefs);
  const query = useQuery({
    queryKey: qk.notificationPrefs,
    queryFn: () => apiGet<NotificationPrefsResponse>("/api/notification-prefs"),
  });
  useEffect(() => {
    if (query.data) {
      const map: Record<string, { muted: boolean; sound: boolean }> = {};
      for (const p of query.data.prefs) map[p.type] = { muted: p.muted, sound: p.sound };
      setPrefs(map);
    }
  }, [query.data, setPrefs]);
  return query;
}

export function useUpdateNotificationPref() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: NotificationPref) => apiPut("/api/notification-prefs", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.notificationPrefs }),
    onError: (e: Error) => toast.error(e.message),
  });
}

export function useNotificationWsSync() {
  const prependNotification = useNotificationStore((s) => s.prependNotification);
  const applyRead = useNotificationStore((s) => s.applyRead);
  const applyReadAll = useNotificationStore((s) => s.applyReadAll);
  const applyCleared = useNotificationStore((s) => s.applyCleared);
  const resolveApproval = useNotificationStore((s) => s.resolveApproval);

  useWsSubscription("notification", (event) => {
    // Muted types never reach here (server skips the broadcast). Among the rest,
    // a sound-off pref means: add + bump badge, but don't trigger the beep.
    const pref = useNotificationStore.getState().prefs[event.data.type];
    prependNotification(event.data, pref?.sound === false);
  });
  useWsSubscription("notification_read", (event) => {
    applyRead(event.data.id, event.data.unread_count);
  });
  useWsSubscription("notifications_read_all", (event) => {
    applyReadAll(event.data.unread_count);
  });
  useWsSubscription("notifications_cleared", () => {
    applyCleared();
  });
  // N7: when an approval is resolved anywhere (toast / channel / another tab),
  // mark its persistent bell row read so it stops lingering unread.
  useWsSubscription("approval_resolved", (event) => {
    resolveApproval(event.approval_id);
  });
}

/**
 * N1 recovery: when the WS transitions disconnected -> connected, any
 * notifications created during the outage exist only in the DB. Refetch the
 * (newest-first, capped) list to reconcile the badge and recent items.
 */
export function useNotificationRecovery() {
  const qc = useQueryClient();
  const connected = useWsStore((s) => s.connected);
  const prev = useRef(connected);
  useEffect(() => {
    if (connected && !prev.current) {
      qc.invalidateQueries({ queryKey: qk.notifications });
    }
    prev.current = connected;
  }, [connected, qc]);
}

// ── Checkpoints ──────────────────────────────────────────────────────────────

export function useCheckpoints(agent: string | null, enabled = true) {
  return useQuery<CheckpointListDto>({
    queryKey: qk.checkpoints(agent ?? ""),
    queryFn: () => listCheckpoints(agent!),
    enabled: !!agent && enabled,
  })
}

export function useRestoreCheckpoint() {
  const qc = useQueryClient()
  return useMutation<RestoreReportDto, Error, { agent: string; n: number; file?: string }>({
    mutationFn: ({ agent, n, file }) => restoreCheckpoint(agent, n, file),
    onSuccess: (_r, { agent }) => qc.invalidateQueries({ queryKey: qk.checkpoints(agent) }),
    onError: (e: Error) => toast.error(e.message),
  })
}

// ── Agent Plan (Stage C initiative) ─────────────────────────────────────────

export function useAgentPlan(agent: string | null) {
  return useQuery<AgentPlan>({
    queryKey: qk.agentPlan(agent ?? ""),
    queryFn: () => getAgentPlan(agent!),
    enabled: !!agent,
  })
}

export function useApproveProposal() {
  const qc = useQueryClient()
  return useMutation<{ ok: boolean; spawned?: boolean; session_id?: string }, Error, { agent: string; id: string }>({
    mutationFn: ({ agent, id }) => approveProposal(agent, id),
    onSuccess: (_r, { agent }) => qc.invalidateQueries({ queryKey: qk.agentPlan(agent) }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useDismissProposal() {
  const qc = useQueryClient()
  return useMutation<{ ok: boolean; changed?: boolean }, Error, { agent: string; id: string }>({
    mutationFn: ({ agent, id }) => dismissProposal(agent, id),
    onSuccess: (_r, { agent }) => qc.invalidateQueries({ queryKey: qk.agentPlan(agent) }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useCancelGoal() {
  const qc = useQueryClient()
  return useMutation<{ ok: boolean; cancelled?: boolean }, Error, { agent: string; sessionId: string }>({
    mutationFn: ({ agent, sessionId }) => cancelGoal(agent, sessionId),
    onSuccess: (_r, { agent }) => qc.invalidateQueries({ queryKey: qk.agentPlan(agent) }),
    onError: (e: Error) => toast.error(e.message),
  })
}

// ── File Handlers ────────────────────────────────────────────────────────────

export function useHandlers() {
  return useQuery({
    queryKey: qk.handlers,
    queryFn: () => apiGet<{ handlers: HandlerAdminRow[] }>("/api/handlers"),
    select: (d) => d.handlers,
    staleTime: 30_000,
  })
}

// Standalone allowlist-view API (the 5 members + enabled). The tab card reads
// `handlers[].enabled` directly (server-merged), so this hook is NOT wired into
// the card — it is exposed for API parity with the backend route and a possible
// future allowlist-only view. Safe to leave unused.
export function useHandlerAllowlist() {
  return useQuery({
    queryKey: qk.handlerAllowlist,
    queryFn: () => apiGet<{ allowlist: HandlerAllowlistRow[] }>("/api/handlers/allowlist"),
    select: (d) => d.allowlist,
    staleTime: 30_000,
  })
}

export function useSetHandlerAllowlist() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { action_ref: string; enabled: boolean }) =>
      apiPut<{ action_ref: string; enabled: boolean }>("/api/handlers/allowlist", data),
    // Optimistic: flip the toggled row's `enabled` immediately (the server derives
    // it, so without this the thumb won't move until the PUT + toolgate-backed
    // refetch completes). Mirrors useSetProviderActive.
    onMutate: async (vars) => {
      await qc.cancelQueries({ queryKey: qk.handlers })
      const snapshot = qc.getQueryData<{ handlers: HandlerAdminRow[] }>(qk.handlers)
      qc.setQueryData<{ handlers: HandlerAdminRow[] }>(qk.handlers, (old) =>
        old
          ? {
              handlers: old.handlers.map((h) =>
                h.id === vars.action_ref ? { ...h, enabled: vars.enabled } : h,
              ),
            }
          : old,
      )
      return { snapshot }
    },
    onError: (e: Error, _vars, ctx) => {
      if (ctx?.snapshot) qc.setQueryData(qk.handlers, ctx.snapshot)
      toast.error(e.message)
    },
    onSettled: () => {
      qc.invalidateQueries({ queryKey: qk.handlerAllowlist })
      qc.invalidateQueries({ queryKey: qk.handlers }) // handlers carry the merged `enabled`
    },
  })
}

export function useHandlerSource(id: string | null) {
  return useQuery({
    queryKey: ["handlers", "source", id],
    queryFn: () => apiGet<HandlerSourceDto>(`/api/handlers/${id}/source`),
    enabled: !!id,
  })
}

export function useCreateHandler() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { id: string; source: string }) =>
      apiPost<{ id: string }>("/api/handlers", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.handlers }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useUpdateHandler() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { id: string; source: string }) =>
      apiPut<{ id: string }>(`/api/handlers/${data.id}`, { source: data.source }),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.handlers }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useDeleteHandler() {
  const qc = useQueryClient()
  return useMutation({
    // NB: apiDelete(path): Promise<void> — it is NOT generic. Do NOT write
    // apiDelete<T>(...) (TS2558). The mutation only invalidates on success.
    mutationFn: (id: string) => apiDelete(`/api/handlers/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.handlers }),
    onError: (e: Error) => toast.error(e.message),
  })
}
