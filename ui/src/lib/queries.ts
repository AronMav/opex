import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query"
import { useEffect } from "react"
import { toast } from "sonner"
import { apiGet, apiPost, apiPut, apiDelete, apiPatch } from "./api"
import { useNotificationStore } from "@/stores/notification-store"
import { useWsSubscription } from "@/hooks/use-ws-subscription"
import type { NotificationsResponse } from "@/types/api"
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
  webhooks: ["webhooks"] as const,
  approvals: ["approvals"] as const,
  backups: ["backups"] as const,
  sessions: (agent: string) => ["sessions", "list", agent] as const,
  sessionMessages: (id: string) => ["sessions", id, "messages"] as const,
  providers: ["providers"] as const,
  providerTypes: ["provider-types"] as const,
  providerActive: ["provider-active"] as const,
  mediaDrivers: ["media-drivers"] as const,
  oauthAccounts: ["oauth", "accounts"] as const,
  oauthBindings: (agent: string) => ["oauth", "bindings", agent] as const,
  notifications: ["notifications"] as const,
  agentTasks: (name: string) => ["agents", name, "tasks"] as const,
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

export function useApprovals() {
  return useQuery({
    queryKey: qk.approvals,
    queryFn: () => apiGet<{ approvals: ApprovalEntry[] }>("/api/approvals"),
    select: (d) => d.approvals ?? [],
    refetchInterval: 5000,
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

export function useSessions(agent: string) {
  return useQuery({
    queryKey: qk.sessions(agent),
    queryFn: () =>
      apiGet<{ sessions: SessionRow[]; total: number }>(
        `/api/sessions?limit=40&agent=${encodeURIComponent(agent)}`
      ),
    enabled: !!agent,
    staleTime: 0, // Always refetch on mount for fresh data
    // No polling needed — session status is server-driven via WS agent_processing events
  })
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
    onError: (e: Error) => toast.error(e.message),
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
    onError: (e: Error) => toast.error(e.message),
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
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useSetProviderActive() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { capability: string; provider_name: string | null }) =>
      apiPut("/api/provider-active", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: qk.providerActive }),
    onError: (e: Error) => toast.error(e.message),
  })
}

export function useSessionMessages(sessionId: string | null, engineRunning = false) {
  return useQuery({
    queryKey: qk.sessionMessages(sessionId!),
    queryFn: () =>
      apiGet<{ messages: MessageRow[] }>(
        `/api/sessions/${sessionId}/messages?limit=100`
      ),
    enabled: !!sessionId,
    staleTime: 2000,
    gcTime: 24 * 60 * 60 * 1000,
    // Poll every 5s when engine is still processing, 3s when streaming
    refetchInterval: (query) => {
      if (engineRunning) return 5000;
      const msgs = (query.state.data as { messages: MessageRow[] } | undefined)?.messages
      return msgs?.some(m => m.status === "streaming") ? 3000 : false
    },
  })
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
  const setNotifications = useNotificationStore((s) => s.setNotifications);
  const query = useQuery({
    queryKey: qk.notifications,
    queryFn: () => apiGet<NotificationsResponse>("/api/notifications?limit=20&offset=0"),
  });
  useEffect(() => {
    if (query.data) {
      setNotifications(query.data.items, query.data.unread_count);
    }
  }, [query.data, setNotifications]);
  return query;
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

export function useNotificationWsSync() {
  const prependNotification = useNotificationStore((s) => s.prependNotification);
  useWsSubscription("notification", (event) => {
    prependNotification(event.data);
  });
}

