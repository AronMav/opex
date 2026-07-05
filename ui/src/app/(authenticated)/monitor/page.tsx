"use client";

import { Suspense, useEffect, useRef, useState, useCallback, memo, Fragment } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { apiGet, apiPost } from "@/lib/api";
import { formatDuration, relativeTime } from "@/lib/format";
import { useAutoRefresh } from "@/hooks/use-auto-refresh";
import { useTranslation } from "@/hooks/use-translation";
import { useWsStore } from "@/stores/ws-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import { useUsage, useDailyUsage, useApprovals, useResolveApproval, useAudit, useSessionFailures, useCuratorRuns } from "@/lib/queries";
import { buildAuditParams } from "./audit-params";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SearchInput } from "@/components/ui/search-input";
import { Switch } from "@/components/ui/switch";
import { ErrorBanner } from "@/components/ui/error-banner";
import { EmptyState } from "@/components/ui/empty-state";
import { CircularLoader } from "@/components/ui/loader";
import { Skeleton } from "@/components/ui/skeleton";
import { Card } from "@/components/ui/card";
import { StatCard } from "@/components/ui/stat-card";
import { StatusBadge } from "@/components/ui/status-badge";
import { SectionHeader } from "@/components/ui/section-header";
import { FilterTabsList } from "@/components/ui/filter-tabs";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Table, TableHeader, TableBody, TableRow, TableHead, TableCell } from "@/components/ui/table";
import { Tabs, TabsContent } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Activity, Clock, Brain, Bot, User, Wrench, Zap, RefreshCw, RotateCcw, Calendar, Database,
  CheckCircle2, XCircle, HeartPulse, AlertTriangle, Stethoscope,
  BarChart3, Cpu, ArrowUpRight, ArrowDownRight, DollarSign,
  ShieldCheck, Check, X, ChevronRight, ScrollText, Sparkles, ClipboardList,
} from "lucide-react";
import type { StatusInfo, StatsInfo, UsageSummary, DailyUsageResponse, AuditEvent, SessionFailureEntry, CuratorRun } from "@/types/api";
import type { LogEntry } from "@/types/api";
import type { WsLog } from "@/types/ws";
import type { TranslationKey } from "@/i18n/types";

// ── Doctor types ────────────────────────────────────────────────────────────

const FIX_ROUTES: Record<string, string> = {
  provider: "/providers",
  secret: "/secrets",
  credential: "/secrets",
  config: "/config",
  channel: "/channels",
  tool: "/tools",
  agent: "/agents",
};

function getFixRoute(hint: string): string | null {
  const lower = hint.toLowerCase();
  for (const [keyword, route] of Object.entries(FIX_ROUTES)) {
    if (lower.includes(keyword)) return route;
  }
  return null;
}

type CheckStatus = "ok" | "warn" | "error";

interface CheckResult {
  status: CheckStatus;
  message: string;
  latency_ms?: number;
  fix_hint?: string;
  details?: unknown;
}

interface DoctorResponse {
  ok: boolean;
  checks: Record<string, CheckResult>;
}

const STATUS_LABEL: Record<CheckStatus, string> = {
  ok: "OK",
  warn: "WARN",
  error: "ERROR",
};

const CHECK_GROUPS: { titleKey: string; keys: string[] }[] = [
  { titleKey: "doctor.group_infrastructure", keys: ["database", "migrations", "pgvector", "memory_worker", "disk", "backup"] },
  { titleKey: "doctor.group_services", keys: ["toolgate", "browser_renderer", "channels"] },
  { titleKey: "doctor.group_providers", keys: ["providers"] },
  { titleKey: "doctor.group_security", keys: ["security_audit", "secrets"] },
  { titleKey: "doctor.group_agents", keys: ["agents", "tool_health"] },
  { titleKey: "doctor.group_network", keys: ["network"] },
];

function FixHintButton({ hint }: { hint: string }) {
  const { t } = useTranslation();
  const router = useRouter();
  const route = getFixRoute(hint);

  return (
    <div className="mt-1 flex items-center gap-2">
      <span className="text-xs text-warning">{hint}</span>
      {route && (
        <Button
          variant="outline"
          size="xs"
          onClick={() => router.push(route)}
        >
          {t("doctor.fix")}
        </Button>
      )}
    </div>
  );
}

function CheckRow({ name, result }: { name: string; result: CheckResult }) {
  const label = STATUS_LABEL[result.status] ?? result.status.toUpperCase();
  const displayName = name.replace(/_/g, " ");

  return (
    <div className="flex flex-col gap-1 border-b py-2 last:border-0 md:flex-row md:items-start md:gap-3">
      <StatusBadge status={result.status} size="sm" className="w-16 shrink-0 justify-center">
        {label}
      </StatusBadge>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-3">
          <span className="text-sm font-medium capitalize">{displayName}</span>
          {result.latency_ms !== undefined && (
            <span className="ml-auto text-xs text-muted-foreground">{result.latency_ms}ms</span>
          )}
        </div>
        <p className="text-sm text-muted-foreground">{result.message}</p>
        {result.fix_hint && <FixHintButton hint={result.fix_hint} />}
      </div>
    </div>
  );
}

function CheckSection({
  title,
  checks,
}: {
  title: string;
  checks: { name: string; result: CheckResult }[];
}) {
  return (
    <Card>
      <div className="border-b border-border px-4 py-3">
        <h2 className="text-sm font-semibold text-foreground">{title}</h2>
      </div>
      <div className="px-4">
        {checks.map(({ name, result }) => (
          <CheckRow key={name} name={name} result={result} />
        ))}
      </div>
    </Card>
  );
}

// ── Watchdog types ──────────────────────────────────────────────────────────

interface ServiceStatus {
  ok: boolean;
  latency_ms: number;
  last_restart: string | null;
  error: string | null;
  flapping: boolean;
  can_restart?: boolean;
}

interface ResourceStatus {
  disk_free_gb: number;
  disk_warning: boolean;
  disk_critical: boolean;
  ram_used_percent: number;
  ram_warning: boolean;
  ram_critical: boolean;
  cpu_load_percent: number;
}

interface ContainerInfo {
  name: string;
  docker_name: string;
  status: string;
  healthy: boolean;
  group: string;
}

interface WatchdogStatus {
  last_check: string;
  uptime_secs: number;
  checks: Record<string, ServiceStatus>;
  resources: ResourceStatus | null;
  containers?: ContainerInfo[];
}

// ── Statistics helpers ──────────────────────────────────────────────────────

const PERIOD_OPTIONS: { value: string; labelKey: TranslationKey }[] = [
  { value: "1", labelKey: "usage.period_24h" },
  { value: "7", labelKey: "usage.period_7d" },
  { value: "30", labelKey: "usage.period_30d" },
  { value: "90", labelKey: "usage.period_90d" },
];

// StatCard accent = chart-token number (1 blue, 2 green, 3 amber, 5 purple).
const METRIC_ACCENT = {
  messages: 1,
  tokens: 2,
  cost: 3,
  sessions: 5,
} as const;

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

// Fill every calendar day between the first and last dated entry so zero-usage
// days appear as gaps-of-zero in the timeline rather than being collapsed out.
function fillDailyGaps(
  sorted: [string, { input: number; output: number; calls: number }][],
): [string, { input: number; output: number; calls: number }][] {
  if (sorted.length < 2) return sorted;
  const byDate = new Map(sorted);
  const first = sorted[0]![0];
  const last = sorted[sorted.length - 1]![0];
  const start = new Date(`${first}T00:00:00Z`);
  const end = new Date(`${last}T00:00:00Z`);
  if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return sorted;
  // Guard against pathological ranges (bad data) — cap at ~2 years of bars.
  const spanDays = Math.round((end.getTime() - start.getTime()) / 86_400_000);
  if (spanDays < 0 || spanDays > 732) return sorted;
  const out: [string, { input: number; output: number; calls: number }][] = [];
  for (let d = new Date(start); d <= end; d.setUTCDate(d.getUTCDate() + 1)) {
    const key = d.toISOString().slice(0, 10);
    out.push([key, byDate.get(key) ?? { input: 0, output: 0, calls: 0 }]);
  }
  return out;
}

const DailyChart = memo(function DailyChart({ data }: { data: DailyUsageResponse["daily"] }) {
  const { t } = useTranslation();
  const byDate = new Map<string, { input: number; output: number; calls: number }>();
  for (const d of data) {
    const existing = byDate.get(d.date) || { input: 0, output: 0, calls: 0 };
    existing.input += d.input_tokens;
    existing.output += d.output_tokens;
    existing.calls += d.call_count;
    byDate.set(d.date, existing);
  }

  const entries = fillDailyGaps(
    Array.from(byDate.entries()).sort(([a], [b]) => a.localeCompare(b)),
  );
  const maxTokens = Math.max(1, ...entries.map(([, v]) => v.input + v.output));

  return (
    <Card className="mb-8">
      <div className="flex items-center gap-3 px-4 sm:px-5 py-4 border-b border-border/50 bg-muted/20 rounded-t-xl">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary/10">
          <Calendar className="h-4 w-4 text-primary" />
        </div>
        <div>
          <h3 className="text-sm font-bold tracking-tight">{t("usage.tokens_by_day")}</h3>
          <span className="text-xs text-muted-foreground">
            <span className="inline-block w-2 h-2 rounded-sm bg-chart-1 mr-1" />{t("usage.input_legend")}
            <span className="inline-block w-2 h-2 rounded-sm bg-chart-2 ml-3 mr-1" />{t("usage.output_legend")}
          </span>
        </div>
      </div>
      <div className="px-4 sm:px-5 py-4">
        <div className="flex gap-2">
          {/* Y-axis: peak / midpoint / zero reference labels */}
          <div className="flex h-40 w-10 shrink-0 flex-col justify-between py-0 text-right text-3xs tabular-nums text-muted-foreground/50">
            <span>{formatTokens(maxTokens)}</span>
            <span>{formatTokens(Math.round(maxTokens / 2))}</span>
            <span>0</span>
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex h-40 items-end gap-0.5 border-l border-border/30 pl-1 sm:gap-1">
              {entries.map(([date, val], idx) => {
                const total = val.input + val.output;
                const pct = (total / maxTokens) * 100;
                const inputPct = total > 0 ? (val.input / total) * 100 : 50;
                const shortDate = date.slice(5);
                const labelStep = Math.max(1, Math.ceil(entries.length / 10));
                const showLabel = entries.length <= 14 || idx % labelStep === 0;

                return (
                  <button
                    type="button"
                    key={date}
                    aria-label={`${date}: ${formatTokens(val.input)} ${t("usage.input_abbr")}, ${formatTokens(val.output)} ${t("usage.output_abbr")}`}
                    className="group relative flex h-full min-w-0 flex-1 cursor-default flex-col justify-end focus:outline-none"
                  >
                    <div className="pointer-events-none absolute bottom-full left-1/2 z-10 mb-2 hidden -translate-x-1/2 group-hover:block group-focus-visible:block">
                      <div className="rounded-lg border border-border bg-popover px-3 py-2 text-xs shadow-lg whitespace-nowrap">
                        <div className="font-bold mb-1">{date}</div>
                        <div className="text-chart-1">{formatTokens(val.input)} {t("usage.input_abbr")}</div>
                        <div className="text-chart-2">{formatTokens(val.output)} {t("usage.output_abbr")}</div>
                        <div className="text-muted-foreground">{t("usage.calls", { count: val.calls })}</div>
                      </div>
                    </div>
                    <div
                      className="w-full overflow-hidden rounded-t-sm transition-all duration-300 group-hover:opacity-80 group-hover:ring-1 group-hover:ring-primary/20 group-focus-visible:ring-1 group-focus-visible:ring-primary/40"
                      style={{ height: `${Math.max(pct, total > 0 ? 2 : 1)}%` }}
                    >
                      <div className="h-full flex flex-col">
                        <div className="bg-chart-1" style={{ flex: inputPct }} />
                        <div className="bg-chart-2" style={{ flex: 100 - inputPct }} />
                      </div>
                    </div>
                    {showLabel ? (
                      <div className="text-3xs text-muted-foreground/60 text-center mt-1 truncate">
                        {shortDate}
                      </div>
                    ) : (
                      <div className="h-3" />
                    )}
                  </button>
                );
              })}
            </div>
          </div>
        </div>
      </div>
    </Card>
  );
});

// ── Audit constants ─────────────────────────────────────────────────────────

const EVENT_TYPES: { value: string; labelKey: TranslationKey }[] = [
  { value: "_all", labelKey: "audit.event_all" },
  { value: "shell_exec", labelKey: "audit.event_shell_exec" },
  { value: "command_blocked", labelKey: "audit.event_command_blocked" },
  { value: "approval_requested", labelKey: "audit.event_approval_requested" },
  { value: "approval_resolved", labelKey: "audit.event_approval_resolved" },
  { value: "prompt_injection_detected", labelKey: "audit.event_prompt_injection" },
];

const EVENT_COLORS: Record<string, string> = {
  shell_exec: "bg-primary/10 text-primary",
  command_blocked: "bg-destructive/10 text-destructive",
  approval_requested: "bg-warning/10 text-warning",
  approval_resolved: "bg-success/10 text-success",
  prompt_injection_detected: "bg-destructive/10 text-destructive font-bold",
};

const AUDIT_PAGE_SIZE = 50;

// ── Failures helpers ────────────────────────────────────────────────────────

const FAILURE_KIND_BADGE: Record<string, string> = {
  sub_agent_timeout: "bg-warning/15 text-warning border-warning/30",
  provider_error: "bg-destructive/10 text-destructive border-destructive/30",
  llm_error: "bg-destructive/10 text-destructive border-destructive/30",
  max_iterations: "bg-warning/15 text-warning border-warning/30",
  tool_error: "bg-warning/15 text-warning border-warning/30",
  other: "bg-muted text-muted-foreground border-border",
};

function failureKindClass(kind: string): string {
  return FAILURE_KIND_BADGE[kind] ?? FAILURE_KIND_BADGE.other!;
}

const LOGS_LEVELS = ["DEBUG", "INFO", "WARN", "ERROR"] as const;
const LEVEL_PRIORITY: Record<string, number> = { DEBUG: 0, INFO: 1, WARN: 2, ERROR: 3 };
const LEVEL_COLORS: Record<string, string> = {
  DEBUG: "text-muted-foreground/60",
  INFO: "text-primary/80",
  WARN: "text-warning/70",
  ERROR: "text-destructive font-bold",
};

// ── Monitor page inner (needs useSearchParams) ──────────────────────────────

function MonitorPageInner() {
  const { t, locale } = useTranslation();
  const searchParams = useSearchParams();
  const router = useRouter();

  const activeTab = searchParams.get("tab") ?? "watchdog";

  const handleTabChange = (value: string) => {
    router.push(`/monitor/?tab=${value}`, { scroll: false });
  };

  // ── Doctor state ──────────────────────────────────────────────────────────

  const { data: doctorData, isLoading: doctorLoading, error: doctorError, refetch: doctorRefetch, isFetching: doctorFetching } = useQuery<DoctorResponse>({
    queryKey: ["doctor"],
    queryFn: () => apiGet<DoctorResponse>("/api/doctor"),
    refetchInterval: 30_000,
  });

  const checks = doctorData?.checks ?? {};
  const allGroupKeys = CHECK_GROUPS.flatMap((g) => g.keys);

  // ── Watchdog state ────────────────────────────────────────────────────────

  const [wdStatus, setWdStatus] = useState<StatusInfo | null>(null);
  const [wdStats, setWdStats] = useState<StatsInfo | null>(null);
  const [watchdog, setWatchdog] = useState<WatchdogStatus | null>(null);
  const [wdError, setWdError] = useState("");
  const [restarting, setRestarting] = useState<string | null>(null);
  const [refreshInterval, setRefreshInterval] = useState(60000);
  const [lastFetch, setLastFetch] = useState<Date | null>(null);
  // Restart confirmation gate (Этап 2): { kind, id, label }
  const [restartConfirm, setRestartConfirm] = useState<
    { kind: "service" | "container"; id: string; label: string } | null
  >(null);

  const restartContainer = async (dockerName: string) => {
    setRestarting(dockerName);
    try { await apiPost(`/api/containers/${dockerName}/restart`); } catch (e) { setWdError(t("watchdog.restart_failed", { error: String(e) })); }
    setTimeout(() => { setRestarting(null); fetchWdData(); }, 3000);
  };

  const restartService = async (name: string) => {
    setRestarting(name);
    try { await apiPost(`/api/watchdog/restart/${name}`); } catch (e) { setWdError(t("watchdog.restart_failed", { error: String(e) })); }
    setTimeout(() => { setRestarting(null); fetchWdData(); }, 5000);
  };

  const confirmRestart = () => {
    if (!restartConfirm) return;
    const { kind, id } = restartConfirm;
    setRestartConfirm(null);
    if (kind === "service") restartService(id);
    else restartContainer(id);
  };


  const fetchWdData = useCallback(async (cancelled?: { current: boolean }) => {
    try {
      const [s, st, wd] = await Promise.all([
        apiGet<StatusInfo>("/api/status"),
        apiGet<StatsInfo>("/api/stats"),
        apiGet<WatchdogStatus>("/api/watchdog/status").catch((e) => { console.warn("[watchdog] status fetch failed:", e); return null; }),
      ]);
      if (cancelled?.current) return;
      setWdStatus(s);
      setWdStats(st);
      if (wd && wd.checks) setWatchdog(wd);
      setLastFetch(new Date());
      setWdError("");
    } catch (e) {
      if (cancelled?.current) return;
      setWdError(`${e}`);
    }
  }, []);

  useEffect(() => {
    const cancelled = { current: false };
    fetchWdData(cancelled);
    return () => { cancelled.current = true; };
  }, [fetchWdData]);
  useAutoRefresh(fetchWdData, refreshInterval);

  const s = wdStatus;
  const st = wdStats;
  const wdChecks = watchdog ? Object.entries(watchdog.checks) : [];
  const allHealthy = wdChecks.length > 0 && wdChecks.every(([, v]) => v.ok);
  const res = watchdog?.resources;

  // ── Statistics state ──────────────────────────────────────────────────────

  const [statsDays, setStatsDays] = useState("30");
  const statsDaysNum = Number(statsDays);
  const { data: usageData, error: usageError, isLoading: usageLoading } = useUsage(statsDaysNum);
  const { data: dailyData, error: dailyError, isLoading: dailyLoading } = useDailyUsage(statsDaysNum);
  const statsIsLoading = usageLoading || dailyLoading;
  const statsError = usageError || dailyError ? `${usageError ?? dailyError}` : "";
  const usage = usageData?.usage ?? [];
  const totalInput = usage.reduce((s, u) => s + u.total_input, 0);
  const totalOutput = usage.reduce((s, u) => s + u.total_output, 0);
  const totalCalls = usage.reduce((s, u) => s + u.call_count, 0);
  const totalTokens = totalInput + totalOutput;
  const totalCost = usage.reduce((s, u) => s + (u.estimated_cost ?? 0), 0);
  const byAgent = new Map<string, UsageSummary[]>();
  for (const u of usage) {
    const arr = byAgent.get(u.agent_id) || [];
    arr.push(u);
    byAgent.set(u.agent_id, arr);
  }
  const maxTotal = Math.max(1, ...usage.map((u) => u.total_input + u.total_output));

  // ── Approvals state ───────────────────────────────────────────────────────

  const { data: approvals = [], isLoading: approvalsLoading, error: approvalsError, refetch: approvalsRefetch } = useApprovals();
  const resolveApproval = useResolveApproval();
  const [actionError, setActionError] = useState("");
  const [processingIds, setProcessingIds] = useState<Set<string>>(new Set());

  const pending = approvals.filter((a) => a.status === "pending");

  const handleResolve = async (id: string, status: "approved" | "rejected") => {
    setActionError("");
    setProcessingIds((prev) => new Set(prev).add(id));
    try {
      await resolveApproval.mutateAsync({ id, status });
    } catch (e) {
      setActionError(`${e}`);
    } finally {
      setProcessingIds((prev) => { const next = new Set(prev); next.delete(id); return next; });
    }
  };

  const approvalsErrorMessage = approvalsError ? `${approvalsError}` : actionError;

  // ── Logs state ────────────────────────────────────────────────────────────

  const ws = useWsStore((s) => s.ws);
  const connected = useWsStore((s) => s.connected);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [logLevel, setLogLevel] = useState("INFO");
  const [logSearch, setLogSearch] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const logsContainerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!ws || !connected || activeTab !== "logs") return;
    ws.send({ type: "subscribe_logs" });
    return () => { ws.send({ type: "unsubscribe_logs" }); };
  }, [ws, connected, activeTab]);

  const onLog = useCallback((m: WsLog) => {
    const entry: LogEntry = {
      level: m.level || "INFO",
      message: m.message || "",
      target: m.target,
      timestamp: m.timestamp || new Date().toISOString(),
    };
    setLogs((prev) => {
      const next = [...prev, entry];
      return next.length > 5000 ? next.slice(-5000) : next;
    });
  }, []);

  useWsSubscription("log", onLog);

  useEffect(() => {
    if (autoScroll && logsContainerRef.current) {
      logsContainerRef.current.scrollTop = logsContainerRef.current.scrollHeight;
    }
  }, [logs, autoScroll]);

  const minPriority = LEVEL_PRIORITY[logLevel] ?? 1;
  const filteredLogs = logs.filter((l) => {
    if ((LEVEL_PRIORITY[l.level] ?? 0) < minPriority) return false;
    if (logSearch && !l.message.toLowerCase().includes(logSearch.toLowerCase())) return false;
    return true;
  });

  // ── Audit state ───────────────────────────────────────────────────────────

  const [auditAgent, setAuditAgent] = useState("_all");
  const [auditEventType, setAuditEventType] = useState("_all");
  const [auditSearch, setAuditSearch] = useState("");
  const [auditOffset, setAuditOffset] = useState(0);
  const [auditAgents, setAuditAgents] = useState<string[]>([]);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const auditParams = buildAuditParams({
    pageSize: AUDIT_PAGE_SIZE,
    offset: auditOffset,
    agent: auditAgent,
    eventType: auditEventType,
    search: auditSearch,
  });

  const { data: auditEvents = [], isFetching: auditLoading, refetch: auditRefetch } = useAudit(auditParams);
  const auditHasMore = auditEvents.length >= AUDIT_PAGE_SIZE;

  const loadAuditAgents = useCallback(async () => {
    try {
      const data = await apiGet<{ agents?: string[] }>("/api/status");
      setAuditAgents(data.agents || []);
    } catch (e) { console.warn("[audit] failed to load agents:", e); }
  }, []);

  useEffect(() => { loadAuditAgents(); }, [loadAuditAgents]);

  useWsSubscription("audit_event", useCallback(() => {
    if (auditOffset === 0) auditRefetch();
  }, [auditOffset, auditRefetch]));

  // ── Failures state ────────────────────────────────────────────────────────

  // The failures endpoint requires a specific agent (no cross-agent bulk list),
  // so default to the first agent once the list loads instead of an "_all" option.
  const [failuresAgent, setFailuresAgent] = useState<string>("");
  const [failuresExpandedId, setFailuresExpandedId] = useState<string | null>(null);
  useEffect(() => {
    if (!failuresAgent && auditAgents.length > 0) setFailuresAgent(auditAgents[0]);
  }, [failuresAgent, auditAgents]);
  const failuresAgentParam = failuresAgent || null;
  const {
    data: failuresData,
    isFetching: failuresLoading,
    refetch: failuresRefetch,
  } = useSessionFailures(failuresAgentParam, 50);
  const failures: SessionFailureEntry[] = failuresData?.failures ?? [];
  const failuresTotal = failuresData?.total ?? 0;

  // ── Render ────────────────────────────────────────────────────────────────

  return (
    <div className="flex h-full flex-col">
      <Tabs value={activeTab} onValueChange={handleTabChange} className="flex flex-col flex-1 min-h-0 min-w-0">
        <div className="border-b border-border/50 bg-background px-4 md:px-6 pt-4 shrink-0 min-w-0">
          <FilterTabsList
            items={[
              { value: "watchdog", label: t("monitor.tab_watchdog"), icon: <ShieldCheck /> },
              { value: "doctor", label: t("monitor.tab_doctor"), icon: <Stethoscope /> },
              { value: "logs", label: t("monitor.tab_logs"), icon: <ScrollText /> },
              { value: "audit", label: t("monitor.tab_audit"), icon: <ClipboardList /> },
              { value: "statistics", label: t("monitor.tab_statistics"), icon: <BarChart3 /> },
              { value: "approvals", label: t("monitor.tab_approvals"), icon: <CheckCircle2 /> },
              { value: "failures", label: t("monitor.tab_failures"), icon: <AlertTriangle /> },
              { value: "curator", label: t("monitor.tab_curator"), icon: <Sparkles /> },
            ]}
          />
        </div>

        {/* Watchdog tab */}
        <TabsContent
          value="watchdog"
          forceMount
          className={activeTab !== "watchdog" ? "hidden" : "flex-1 overflow-y-auto"}
        >
          <div className="p-4 md:p-6 lg:p-8 space-y-8">
            {/* Watchdog section */}
            <div>
              <SectionHeader
                icon={HeartPulse}
                title={t("watchdog.title")}
                description={t("watchdog.subtitle")}
                actions={
                  <div className="flex flex-wrap items-center gap-3">
                    {lastFetch && (
                      <span className="text-3xs text-muted-foreground tabular-nums">
                        {lastFetch.toLocaleTimeString()}
                      </span>
                    )}
                    <Select value={String(refreshInterval)} onValueChange={(v) => setRefreshInterval(Number(v))}>
                      <SelectTrigger className="h-9 w-20 text-xs bg-card/50 border-border">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="5000" className="text-xs">5s</SelectItem>
                        <SelectItem value="15000" className="text-xs">15s</SelectItem>
                        <SelectItem value="30000" className="text-xs">30s</SelectItem>
                        <SelectItem value="60000" className="text-xs">60s</SelectItem>
                      </SelectContent>
                    </Select>
                    {res && (
                      <div className="flex flex-wrap items-center gap-4 bg-muted/20 px-4 py-2 rounded-lg border border-border">
                        <div className="flex items-center gap-1.5">
                          <span className="text-3xs text-muted-foreground">{t("watchdog.resource.disk")}</span>
                          <span className={`font-mono text-sm font-bold ${res.disk_critical ? "text-destructive" : res.disk_warning ? "text-warning" : "text-foreground"}`}>{res.disk_free_gb.toFixed(0)}GB</span>
                        </div>
                        <div className="w-px h-4 bg-border/50" />
                        <div className="flex items-center gap-1.5">
                          <span className="text-3xs text-muted-foreground">{t("watchdog.resource.ram")}</span>
                          <span className={`font-mono text-sm font-bold ${res.ram_critical ? "text-destructive" : res.ram_warning ? "text-warning" : "text-foreground"}`}>{res.ram_used_percent.toFixed(0)}%</span>
                        </div>
                        {res.cpu_load_percent != null && (
                          <>
                            <div className="w-px h-4 bg-border/50" />
                            <div className="flex items-center gap-1.5">
                              <span className="text-3xs text-muted-foreground">{t("watchdog.resource.cpu")}</span>
                              <span className={`font-mono text-sm font-bold ${res.cpu_load_percent > 90 ? "text-destructive" : res.cpu_load_percent > 70 ? "text-warning" : "text-foreground"}`}>{res.cpu_load_percent.toFixed(0)}%</span>
                            </div>
                          </>
                        )}
                      </div>
                    )}
                  </div>
                }
              />

              {wdError && <ErrorBanner error={wdError} />}

              {(() => {
                const valueSkeleton = <Skeleton className="h-7 w-16" />;
                const wdOk = wdChecks.length > 0
                  ? (allHealthy && (!watchdog?.containers || watchdog.containers.every(c => c.healthy)))
                  : (s?.status === "ok");
                const statusText = wdChecks.length > 0
                  ? (wdOk ? "OK" : "ISSUES")
                  : s?.status?.toUpperCase();
                const statusValue = statusText
                  ? <span className={wdOk ? undefined : "text-destructive"}>{statusText}</span>
                  : valueSkeleton;
                return (
                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-3 md:gap-5 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
                    <StatCard
                      label={t("dashboard.status")}
                      value={statusValue}
                      sub={s?.version}
                      icon={Activity}
                      accent={wdOk ? 2 : undefined}
                    />
                    <StatCard label={t("dashboard.uptime")} value={s ? formatDuration(s.uptime_seconds) : valueSkeleton} sub={t("dashboard.uptime_sub")} icon={Clock} />
                    <StatCard label={t("dashboard.memory")} value={s?.memory_chunks?.toLocaleString() ?? valueSkeleton} sub={t("dashboard.memory_sub")} icon={Brain} />
                    <StatCard label={t("dashboard.agents")} value={String(s?.agents?.length ?? "0")} sub={s?.agents?.join(", ") || t("dashboard.agents_none")} icon={Bot} />
                    <StatCard label={t("dashboard.sessions")} value={String(s?.active_sessions ?? "0")} sub={t("dashboard.sessions_sub")} icon={User} />
                    <StatCard label={t("dashboard.tools")} value={String(s?.tools_registered ?? "0")} sub={t("dashboard.tools_sub")} icon={Wrench} />
                    <StatCard label={t("dashboard.messages_today")} value={String(st?.messages_today ?? "0")} sub={t("dashboard.total_messages", { value: st?.total_messages.toLocaleString() ?? "0" })} icon={Zap} />
                    <StatCard label={t("dashboard.sessions_today")} value={String(st?.sessions_today ?? "0")} sub={t("dashboard.total_sessions", { value: st?.total_sessions.toLocaleString() ?? "0" })} icon={RefreshCw} />
                    <StatCard label={t("dashboard.scheduled_jobs")} value={String(s?.scheduled_jobs ?? "0")} sub={t("dashboard.scheduled_sub")} icon={Calendar} />
                  </div>
                );
              })()}


              {wdChecks.length > 0 && (
                <div className="mt-8">
                  <div className="flex items-center gap-3 mb-4">
                    <HeartPulse size={16} className="text-primary/50" />
                    <span className="text-sm font-semibold text-foreground">{t("watchdog.services")}</span>
                    <Badge variant="outline" size="sm" className="font-mono">
                      {wdChecks.filter(([,v]) => v.ok).length}/{wdChecks.length}
                    </Badge>
                  </div>
                  <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-3">
                    {wdChecks.map(([name, svc]) => (
                      <Card
                        key={name}
                        className={`p-3 flex flex-col gap-1.5 min-w-0 overflow-hidden ${
                          svc.flapping ? "border-l-2 border-l-warning" : !svc.ok ? "border-l-2 border-l-destructive" : ""
                        }`}
                      >
                        <div className="flex items-center justify-between gap-1">
                          <div className="flex items-center gap-1.5 min-w-0">
                            {svc.ok ? (
                              <CheckCircle2 size={14} className="text-success shrink-0" />
                            ) : svc.flapping ? (
                              <AlertTriangle size={14} className="text-warning shrink-0" />
                            ) : (
                              <XCircle size={14} className="text-destructive shrink-0" />
                            )}
                            <span className="text-xs font-semibold text-muted-foreground truncate">{name}</span>
                          </div>
                          {svc.can_restart && (
                            <Button
                              variant="outline"
                              size="icon-sm"
                              onClick={() => setRestartConfirm({ kind: "service", id: name, label: name })}
                              disabled={restarting === name}
                              aria-label={t("watchdog.restart_service")}
                              className="tap-target"
                            >
                              <RotateCcw className={`h-4 w-4 text-muted-foreground ${restarting === name ? "animate-spin" : ""}`} />
                            </Button>
                          )}
                        </div>
                        <span className="font-mono text-xs text-foreground/80">{svc.latency_ms}ms</span>
                        {svc.error && (
                          <span className="text-3xs text-destructive truncate" title={svc.error}>{svc.error}</span>
                        )}
                        {svc.flapping && (
                          <Badge variant="outline-warning" size="sm" className="w-fit">{t("watchdog.flapping")}</Badge>
                        )}
                        {svc.last_restart && (
                          <Badge variant="outline-warning" size="sm" className="w-fit">{t("watchdog.restarted")}</Badge>
                        )}
                      </Card>
                    ))}
                  </div>
                </div>
              )}

              {watchdog?.containers && watchdog.containers.length > 0 && (() => {
                const agents = watchdog.containers.filter(c => c.group === "agent");
                const infra = watchdog.containers.filter(c => c.group !== "agent");
                return (
                  <div className="mt-8 space-y-6">
                    {infra.length > 0 && (
                      <div>
                        <div className="flex items-center gap-3 mb-3">
                          <Database size={16} className="text-primary/50" />
                          <span className="text-sm font-semibold text-foreground">{t("watchdog.infrastructure")}</span>
                          <Badge variant="outline" size="sm" className="font-mono">
                            {infra.filter(c => c.healthy).length}/{infra.length}
                          </Badge>
                        </div>
                        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-3">
                          {infra.map((c) => (
                            <Card key={c.name} className={`px-3 py-2.5 flex items-center gap-2 group min-w-0 overflow-hidden ${!c.healthy ? "border-l-2 border-l-destructive bg-destructive/10" : ""}`}>
                              <span className={`h-3 w-3 rounded-full shrink-0 ${c.healthy ? "bg-success" : "bg-destructive"}`} />
                              <div className="min-w-0 flex-1">
                                <span className="text-xs font-semibold text-foreground block">{c.name}</span>
                                <span className={`text-3xs block ${c.healthy ? "text-muted-foreground" : "text-destructive"}`}>{c.status}</span>
                              </div>
                              <Button
                                variant="outline"
                                size="icon-sm"
                                onClick={() => setRestartConfirm({ kind: "container", id: c.docker_name, label: c.name })}
                                disabled={restarting === c.docker_name}
                                aria-label={t("watchdog.restart_service")}
                                className="tap-target shrink-0"
                              >
                                <RotateCcw className={`h-4 w-4 text-muted-foreground ${restarting === c.docker_name ? "animate-spin" : ""}`} />
                              </Button>
                            </Card>
                          ))}
                        </div>
                      </div>
                    )}
                    {agents.length > 0 && (
                      <div>
                        <div className="flex items-center gap-3 mb-3">
                          <Bot size={16} className="text-primary/50" />
                          <span className="text-sm font-semibold text-foreground">{t("watchdog.agents")}</span>
                          <Badge variant="outline" size="sm" className="font-mono">
                            {agents.filter(c => c.healthy).length}/{agents.length}
                          </Badge>
                        </div>
                        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-3">
                          {agents.map((c) => (
                            <Card key={c.name} className={`px-3 py-2.5 flex items-center gap-2 group min-w-0 overflow-hidden ${!c.healthy ? "border-l-2 border-l-destructive bg-destructive/10" : ""}`}>
                              <span className={`h-3 w-3 rounded-full shrink-0 ${c.healthy ? "bg-success" : "bg-destructive"}`} />
                              <div className="min-w-0 flex-1">
                                <span className="text-xs font-semibold text-foreground block">{c.name}</span>
                                <span className={`text-3xs block ${c.healthy ? "text-muted-foreground" : "text-destructive"}`}>{c.status}</span>
                              </div>
                              <Button
                                variant="outline"
                                size="icon-sm"
                                onClick={() => setRestartConfirm({ kind: "container", id: c.docker_name, label: c.name })}
                                disabled={restarting === c.docker_name}
                                aria-label={t("watchdog.restart_service")}
                                className="tap-target shrink-0"
                              >
                                <RotateCcw className={`h-4 w-4 text-muted-foreground ${restarting === c.docker_name ? "animate-spin" : ""}`} />
                              </Button>
                            </Card>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>
                );
              })()}
            </div>
          </div>
        </TabsContent>

        {/* Doctor tab */}
        <TabsContent
          value="doctor"
          forceMount
          className={activeTab !== "doctor" ? "hidden" : "flex-1 overflow-y-auto"}
        >
          <div className="p-4 md:p-6 lg:p-8 space-y-8">
            <div>
              <SectionHeader
                icon={Stethoscope}
                title={t("doctor.title")}
                description={t("doctor.subtitle")}
                actions={
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => doctorRefetch()}
                    disabled={doctorFetching}
                  >
                    <RefreshCw size={14} className={doctorFetching ? "animate-spin mr-2" : "mr-2"} />
                    {t("common.refresh")}
                  </Button>
                }
              />

              {doctorData && (
                <div
                  className={`rounded-md p-3 text-sm font-medium mb-4 ${
                    doctorData.ok
                      ? "border border-success/40 bg-success/10 text-success"
                      : "border border-destructive/50 bg-destructive/10 text-destructive"
                  }`}
                >
                  {doctorData.ok ? t("doctor.all_ok") : t("doctor.needs_attention")}
                </div>
              )}

              {doctorLoading && (
                <div className="space-y-4">
                  {[1, 2, 3].map((i) => (
                    <Skeleton key={i} className="h-24 rounded-xl" />
                  ))}
                </div>
              )}

              {doctorError && (
                <ErrorBanner error={t("doctor.error")} onRetry={doctorRefetch} />
              )}

              <div className="space-y-4">
                {CHECK_GROUPS.map((group) => {
                  const groupChecks = group.keys
                    .filter((k) => k in checks)
                    .map((k) => ({ name: k, result: checks[k] }));
                  if (groupChecks.length === 0) return null;
                  return (
                    <CheckSection key={group.titleKey} title={t(group.titleKey as Parameters<typeof t>[0])} checks={groupChecks} />
                  );
                })}
                {Object.entries(checks).filter(([k]) => !allGroupKeys.includes(k)).length > 0 && (
                  <CheckSection
                    title={t("doctor.group_other")}
                    checks={Object.entries(checks)
                      .filter(([k]) => !allGroupKeys.includes(k))
                      .map(([name, result]) => ({ name, result }))}
                  />
                )}
              </div>
            </div>
          </div>
        </TabsContent>

        {/* Logs tab */}
        <TabsContent
          value="logs"
          forceMount
          className={activeTab !== "logs" ? "hidden" : "flex-1 overflow-hidden min-h-0"}
        >
          {/* Logs section */}
          <div className="flex h-full flex-col bg-background selection:bg-primary/20 min-h-0">
            <div className="z-10 flex flex-col md:flex-row md:items-center gap-4 border-b border-border/50 bg-background px-4 py-3 md:px-6 md:h-16 shrink-0">
              <div className="flex flex-col gap-0.5 md:mr-4">
                <h2 className="font-display text-lg font-bold tracking-tight text-foreground">{t("logs.title")}</h2>
                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                  <span className={`h-1.5 w-1.5 rounded-full ${connected ? "bg-success" : "bg-destructive"}`} />
                  {connected ? t("logs.connected") : t("logs.disconnected")}
                </div>
              </div>

              <div className="flex flex-wrap items-center gap-2 md:gap-3 flex-1">
                <Select value={logLevel} onValueChange={setLogLevel}>
                  <SelectTrigger className="h-9 min-w-20 sm:min-w-28 border-border bg-card/50 font-mono text-sm rounded-lg">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent className="border-border rounded-lg">
                    {LOGS_LEVELS.map((l) => (
                      <SelectItem key={l} value={l} className="font-mono text-sm">{l}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>

                <Input
                  placeholder={t("logs.search_placeholder")}
                  value={logSearch}
                  onChange={(e) => setLogSearch(e.target.value)}
                  className="h-9 flex-1 md:w-48 md:flex-none border-border bg-card/50 font-mono text-sm placeholder:text-muted-foreground/60 rounded-lg focus:ring-primary/20"
                />

                <div className="hidden sm:flex items-center gap-3 px-3 py-1.5 rounded-lg bg-card/50 border border-border">
                  <span className="text-xs text-muted-foreground">{t("logs.autoscroll")}</span>
                  <Switch checked={autoScroll} onCheckedChange={setAutoScroll} className="data-[state=checked]:bg-primary" />
                </div>
              </div>

              <div className="flex flex-wrap items-center justify-between md:justify-end gap-2 sm:gap-4 w-full md:w-auto mt-2 md:mt-0">
                <div className="flex sm:hidden items-center gap-2">
                  <Switch checked={autoScroll} onCheckedChange={setAutoScroll} className="scale-75 data-[state=checked]:bg-primary" />
                  <span className="text-xs text-muted-foreground">{t("logs.autoscroll_short")}</span>
                </div>
                <div className="flex flex-wrap items-center gap-2 sm:gap-4">
                  <div className="font-mono text-xs tabular-nums text-muted-foreground">
                    {t("logs.entries_count", { count: filteredLogs.length })}
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-9 text-xs"
                    onClick={() => {
                      const text = filteredLogs.map((l: { timestamp: string; level: string; target?: string; message: string }) =>
                        `${l.timestamp} [${l.level}]${l.target ? ` [${l.target}]` : ""} ${l.message}`
                      ).join("\n");
                      const blob = new Blob([text], { type: "text/plain" });
                      const url = URL.createObjectURL(blob);
                      const a = document.createElement("a");
                      a.href = url;
                      a.download = `opex-logs-${new Date().toISOString().slice(0,10)}.txt`;
                      a.click();
                      URL.revokeObjectURL(url);
                    }}
                    disabled={filteredLogs.length === 0}
                  >
                    {t("logs.download")}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-9 text-xs text-muted-foreground hover:text-destructive hover:border-destructive/50 hover:bg-destructive/10"
                    onClick={() => setLogs([])}
                  >
                    {t("logs.clear")}
                  </Button>
                </div>
              </div>
            </div>

            <div
              ref={logsContainerRef}
              className="flex-1 overflow-y-auto p-4 md:p-6 font-mono text-sm leading-relaxed scrollbar-thin"
            >
              {filteredLogs.length === 0 ? (
                <div className="flex h-full flex-col items-center justify-center gap-4 opacity-40">
                  <div className="h-16 w-px bg-gradient-to-b from-transparent via-primary/50 to-transparent" />
                  <p className="text-sm text-muted-foreground">{t("logs.waiting")}</p>
                </div>
              ) : (
                <div className="flex flex-col gap-1">
                  {filteredLogs.map((l, i) => (
                    <div
                      key={i}
                      className="group flex flex-col md:flex-row gap-1 md:gap-4 py-1.5 px-3 rounded-md transition-colors hover:bg-muted/30"
                      style={{ contentVisibility: "auto", containIntrinsicSize: "auto 24px" }}
                    >
                      <div className="flex items-center gap-3 shrink-0">
                        <span className="w-16 text-muted-foreground/60 tabular-nums group-hover:text-muted-foreground/60 transition-colors">
                          {new Date(l.timestamp).toLocaleTimeString(locale === "en" ? "en-US" : "ru-RU", { hour12: false })}
                        </span>
                        <span className={`w-12 font-bold uppercase tracking-tighter ${LEVEL_COLORS[l.level] || ""}`}>
                          {l.level}
                        </span>
                        {l.target && (
                          <span className="w-24 md:w-32 truncate text-primary/50 font-bold hidden sm:inline-block" title={l.target}>
                            [{l.target}]
                          </span>
                        )}
                      </div>
                      {l.target && (
                        <span className="text-primary/50 font-bold sm:hidden" title={l.target}>
                          [{l.target}]
                        </span>
                      )}
                      <span className="min-w-0 break-all text-foreground/80 group-hover:text-foreground transition-colors ml-4 md:ml-0">
                        {l.message}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>

        </TabsContent>

        {/* Audit tab */}
        <TabsContent
          value="audit"
          forceMount
          className={activeTab !== "audit" ? "hidden" : "flex-1 overflow-hidden min-h-0"}
        >
          <div className="flex h-full flex-col bg-background selection:bg-primary/20 min-h-0">
            <div className="z-10 flex flex-col md:flex-row md:items-center gap-4 border-b border-border/50 bg-background px-4 py-3 md:px-6 md:h-16 shrink-0">
              <div className="flex flex-col gap-0.5 md:mr-4">
                <h2 className="font-display text-lg font-bold tracking-tight text-foreground">{t("audit.title")}</h2>
                <p className="text-sm text-muted-foreground">{t("audit.subtitle")}</p>
              </div>

              <div className="flex flex-wrap items-center gap-2 md:gap-3 flex-1">
                <Select value={auditAgent} onValueChange={(v) => { setAuditAgent(v); setAuditOffset(0); }}>
                  <SelectTrigger className="h-9 min-w-24 sm:min-w-32 border-border bg-card/50 text-sm rounded-lg">
                    <SelectValue placeholder={t("audit.agent_placeholder")} />
                  </SelectTrigger>
                  <SelectContent className="border-border rounded-lg">
                    <SelectItem value="_all" className="text-sm">{t("audit.all_agents")}</SelectItem>
                    {auditAgents.map((a) => (
                      <SelectItem key={a} value={a} className="text-sm">{a}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>

                <Select value={auditEventType} onValueChange={(v) => { setAuditEventType(v); setAuditOffset(0); }}>
                  <SelectTrigger className="h-9 min-w-24 sm:min-w-36 border-border bg-card/50 text-sm rounded-lg">
                    <SelectValue placeholder={t("audit.event_type_placeholder")} />
                  </SelectTrigger>
                  <SelectContent className="border-border rounded-lg">
                    {EVENT_TYPES.map((et) => (
                      <SelectItem key={et.value} value={et.value} className="text-sm">{t(et.labelKey)}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>

                <SearchInput
                  placeholder={t("audit.search_placeholder")}
                  value={auditSearch}
                  onChange={(v) => { setAuditSearch(v); setAuditOffset(0); }}
                  debounceMs={350}
                  className="flex-1 md:w-56 md:flex-none"
                />
              </div>

              <div className="flex items-center gap-4 shrink-0">
                <span className="font-mono text-xs tabular-nums text-muted-foreground hidden md:inline">
                  {t("audit.events_count", { count: auditEvents.length })}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => auditRefetch()}
                  disabled={auditLoading}
                >
                  {t("common.refresh")}
                </Button>
              </div>
            </div>

            <div className="flex-1 overflow-y-auto p-4 md:p-6 scrollbar-thin">
              {auditLoading && auditEvents.length === 0 ? (
                <div className="flex h-full items-center justify-center">
                  <CircularLoader size="lg" />
                </div>
              ) : auditEvents.length === 0 ? (
                <EmptyState icon={ScrollText} text={t("audit.no_events")} />
              ) : (
                <div className="flex flex-col gap-2">
                  {auditEvents.map((e: AuditEvent) => (
                    <div
                      key={e.id}
                      className="group rounded-lg border border-border/50 bg-card/30 transition-colors hover:bg-card/50"
                    >
                      <button
                        type="button"
                        className="flex w-full flex-col gap-1 px-4 py-3 text-left md:flex-row md:flex-wrap md:items-center md:gap-3"
                        onClick={() => setExpandedId(expandedId === e.id ? null : e.id)}
                      >
                        <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground-subtle md:w-20">
                          {new Date(e.created_at).toLocaleTimeString(locale === "en" ? "en-US" : "ru-RU", { hour12: false })}
                        </span>
                        <span className="shrink-0 font-mono text-xs text-muted-foreground truncate md:w-20" title={e.agent_id}>
                          {e.agent_id}
                        </span>
                        <span className={`w-fit shrink-0 rounded-md px-2 py-0.5 text-xs font-medium ${EVENT_COLORS[e.event_type] || "bg-muted text-muted-foreground"}`}>
                          {e.event_type}
                        </span>
                        {e.actor && (
                          <span className="text-xs text-muted-foreground-subtle truncate">
                            {t("audit.from", { actor: e.actor })}
                          </span>
                        )}
                        <span className="text-xs text-muted-foreground/50 md:ml-auto">
                          {new Date(e.created_at).toLocaleDateString(locale === "en" ? "en-US" : "ru-RU")}
                        </span>
                        <ChevronRight className="hidden text-muted-foreground/50 transition-transform h-4 w-4 md:block" style={{ transform: expandedId === e.id ? "rotate(90deg)" : "rotate(0)" }} />
                      </button>

                      {expandedId === e.id && (
                        <div className="border-t border-border/30 px-4 py-3">
                          <pre className="overflow-x-auto rounded-md bg-muted/50 p-3 font-mono text-xs text-foreground/80 whitespace-pre-wrap break-all">
                            {JSON.stringify(e.details, null, 2)}
                          </pre>
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              )}
            </div>

            {(auditOffset > 0 || auditHasMore) && (
              <div className="flex items-center justify-center gap-3 border-t border-border/50 bg-background px-4 py-3 shrink-0">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={auditOffset === 0 || auditLoading}
                  onClick={() => setAuditOffset(Math.max(0, auditOffset - AUDIT_PAGE_SIZE))}
                >
                  {t("common.back")}
                </Button>
                <span className="font-mono text-xs tabular-nums text-muted-foreground">
                  {auditOffset + 1}–{auditOffset + auditEvents.length}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!auditHasMore || auditLoading}
                  onClick={() => setAuditOffset(auditOffset + AUDIT_PAGE_SIZE)}
                >
                  {t("common.forward")}
                </Button>
              </div>
            )}
          </div>
        </TabsContent>

        {/* Statistics tab */}
        <TabsContent
          value="statistics"
          forceMount
          className={activeTab !== "statistics" ? "hidden" : "flex-1 overflow-y-auto"}
        >
          <div className="p-4 md:p-6 lg:p-8 selection:bg-primary/20">
            {statsIsLoading ? (
              <div className="space-y-6">
                {[1, 2, 3].map((i) => (
                  <Skeleton key={i} className="h-32 rounded-xl" />
                ))}
              </div>
            ) : (
              <>
                <SectionHeader
                  icon={BarChart3}
                  title={t("usage.title")}
                  description={t("usage.subtitle")}
                  actions={
                    <Select value={statsDays} onValueChange={setStatsDays}>
                      <SelectTrigger className="w-full sm:w-44 h-9 bg-card/50 border-border text-sm">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent className="border-border">
                        {PERIOD_OPTIONS.map((o) => (
                          <SelectItem key={o.value} value={o.value}>{t(o.labelKey)}</SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  }
                />

                {statsError && <ErrorBanner error={statsError} />}

                {totalTokens > 0 && (
                  <Card className="mb-5 border-chart-3/20 p-5 flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between overflow-hidden relative">
                    <div className="absolute -right-6 -top-6 opacity-5">
                      <Zap className="h-32 w-32 text-chart-3" />
                    </div>
                    <div className="relative min-w-0">
                      <div className="flex items-center gap-2 mb-1">
                        <Zap className="h-4 w-4 text-chart-3" />
                        <span className="text-xs font-medium text-muted-foreground uppercase tracking-wide">{t("usage.total_tokens_summary")}</span>
                      </div>
                      <div className="text-4xl font-display font-bold tracking-tight text-chart-3">
                        {formatTokens(totalTokens)}
                      </div>
                      <div className="text-xs text-muted-foreground-subtle mt-1">
                        {t("usage.period_days", { days: usageData?.days ?? 0 })} &middot; {totalCalls.toLocaleString()} {t("usage.calls_short")}
                      </div>
                    </div>
                    <div className="relative flex items-end gap-3 sm:flex-col sm:gap-1 sm:shrink-0">
                      <div className="text-right">
                        <div className="text-xs text-muted-foreground-subtle">{t("usage.input_short")}</div>
                        <div className="text-sm font-mono font-bold text-chart-1">{formatTokens(totalInput)}</div>
                      </div>
                      <div className="text-right">
                        <div className="text-xs text-muted-foreground-subtle">{t("usage.output_short")}</div>
                        <div className="text-sm font-mono font-bold text-chart-2">{formatTokens(totalOutput)}</div>
                      </div>
                      {totalCost > 0 && (
                        <div className="text-right">
                          <div className="text-xs text-muted-foreground-subtle">{t("usage.estimated_cost")}</div>
                          <div className="text-sm font-mono font-bold text-chart-5">${totalCost.toFixed(4)}</div>
                        </div>
                      )}
                    </div>
                  </Card>
                )}

                <div className="grid grid-cols-2 md:grid-cols-4 gap-3 mb-8">
                  <StatCard
                    icon={Zap}
                    label={t("usage.total_tokens")}
                    value={formatTokens(totalTokens)}
                    sub={t("usage.period_days", { days: usageData?.days ?? 0 })}
                    accent={METRIC_ACCENT.cost}
                  />
                  <StatCard
                    icon={ArrowUpRight}
                    label={t("usage.input_tokens")}
                    value={formatTokens(totalInput)}
                    sub={t("usage.pct_of_total", { pct: ((totalInput / Math.max(totalTokens, 1)) * 100).toFixed(0) })}
                    accent={METRIC_ACCENT.messages}
                  />
                  <StatCard
                    icon={ArrowDownRight}
                    label={t("usage.output_tokens")}
                    value={formatTokens(totalOutput)}
                    sub={t("usage.pct_of_total", { pct: ((totalOutput / Math.max(totalTokens, 1)) * 100).toFixed(0) })}
                    accent={METRIC_ACCENT.tokens}
                  />
                  <StatCard
                    icon={DollarSign}
                    label={t("usage.estimated_cost")}
                    value={totalCost > 0 ? `$${totalCost.toFixed(4)}` : "$0"}
                    sub={t("usage.api_calls", { count: totalCalls.toLocaleString() })}
                    accent={METRIC_ACCENT.sessions}
                  />
                </div>

                {dailyData && dailyData.daily.length > 0 && <DailyChart data={dailyData.daily} />}

                {usage.length === 0 ? (
                  <EmptyState
                    icon={BarChart3}
                    text={t("usage.no_data")}
                    hint={<p className="text-xs mt-1 opacity-60">{t("usage.tracking_hint")}</p>}
                  />
                ) : (
                  <div className="space-y-6">
                    {Array.from(byAgent.entries()).map(([agent, rows]) => {
                      const agentInput = rows.reduce((s, r) => s + r.total_input, 0);
                      const agentOutput = rows.reduce((s, r) => s + r.total_output, 0);
                      const agentCalls = rows.reduce((s, r) => s + r.call_count, 0);

                      return (
                        <Card key={agent} className="overflow-hidden min-w-0">
                          <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-3 px-4 sm:px-5 py-4 border-b border-border/50 bg-muted/20">
                            <div className="flex items-center gap-3">
                              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary/10">
                                <Cpu className="h-4 w-4 text-primary" />
                              </div>
                              <div className="min-w-0">
                                <h3 className="text-sm font-bold tracking-tight truncate">{agent}</h3>
                                <span className="text-xs text-muted-foreground">
                                  {t("usage.calls", { count: agentCalls.toLocaleString() })}
                                </span>
                              </div>
                            </div>
                            <div className="flex flex-wrap gap-4 sm:gap-6 text-right sm:ml-0">
                              <div>
                                <div className="text-xs text-muted-foreground">{t("usage.input_short")}</div>
                                <div className="text-sm font-mono font-bold text-chart-1">{formatTokens(agentInput)}</div>
                              </div>
                              <div>
                                <div className="text-xs text-muted-foreground">{t("usage.output_short")}</div>
                                <div className="text-sm font-mono font-bold text-chart-2">{formatTokens(agentOutput)}</div>
                              </div>
                              <div>
                                <div className="text-xs text-muted-foreground">{t("usage.total_short")}</div>
                                <div className="text-sm font-mono font-bold">{formatTokens(agentInput + agentOutput)}</div>
                              </div>
                            </div>
                          </div>

                          <Table>
                            <TableBody>
                              {rows.map((row) => {
                                const rowTotal = row.total_input + row.total_output;
                                const pct = (rowTotal / maxTotal) * 100;

                                return (
                                  <TableRow key={`${row.agent_id}-${row.provider}-${row.model}`} className="relative">
                                    <TableCell className="relative px-4 py-3 sm:px-5">
                                      <div
                                        className="absolute inset-y-0 left-0 bg-primary/5 transition-all duration-500"
                                        style={{ width: `${pct}%` }}
                                      />
                                      <div className="relative flex flex-col sm:flex-row sm:items-center justify-between gap-1.5 sm:gap-3">
                                        <div className="flex items-center gap-3 flex-wrap">
                                          <span className="inline-flex h-6 items-center rounded-md bg-muted/50 px-2 text-xs font-mono font-medium text-muted-foreground">
                                            {row.provider}
                                          </span>
                                          {row.model && (
                                            <span className="inline-flex h-6 items-center rounded-md bg-primary/10 px-2 text-xs font-mono font-medium text-primary/80">
                                              {row.model}
                                            </span>
                                          )}
                                          <span className="text-xs text-muted-foreground-subtle">
                                            {t("usage.calls", { count: row.call_count.toLocaleString() })}
                                          </span>
                                        </div>
                                        <div className="flex flex-wrap gap-3 sm:gap-5 text-right ml-0 sm:ml-auto">
                                          <span className="text-xs font-mono tabular-nums text-chart-1/80">
                                            {formatTokens(row.total_input)} {t("usage.input_abbr")}
                                          </span>
                                          <span className="text-xs font-mono tabular-nums text-chart-2/80">
                                            {formatTokens(row.total_output)} {t("usage.output_abbr")}
                                          </span>
                                          <span className="text-xs font-mono font-bold tabular-nums">
                                            {formatTokens(rowTotal)}
                                          </span>
                                          {row.estimated_cost != null && (
                                            <span className="text-xs font-mono tabular-nums text-chart-5/80">
                                              ${row.estimated_cost.toFixed(4)}
                                            </span>
                                          )}
                                        </div>
                                      </div>
                                    </TableCell>
                                  </TableRow>
                                );
                              })}
                            </TableBody>
                          </Table>
                        </Card>
                      );
                    })}
                  </div>
                )}
              </>
            )}
          </div>
        </TabsContent>

        {/* Approvals tab */}
        <TabsContent
          value="approvals"
          forceMount
          className={activeTab !== "approvals" ? "hidden" : "flex-1 overflow-y-auto"}
        >
          <div className="p-4 md:p-6 lg:p-8 selection:bg-primary/20">
            <SectionHeader
              icon={ShieldCheck}
              title={t("approvals.title")}
              description={t("approvals.subtitle")}
              actions={
                <Button variant="outline" size="sm" onClick={() => approvalsRefetch()} disabled={approvalsLoading}>
                  <RefreshCw className={`mr-2 h-4 w-4 ${approvalsLoading ? "animate-spin" : ""}`} />
                  {t("common.refresh")}
                </Button>
              }
            />

            {approvalsErrorMessage && <ErrorBanner error={approvalsErrorMessage} />}

            {pending.length === 0 ? (
              <EmptyState icon={ShieldCheck} text={t("approvals.no_pending")} />
            ) : (
              <div className="grid gap-4 md:gap-6">
                {pending.map((a) => (
                  <Card
                    key={a.id}
                    interactive
                    className="group relative flex flex-col gap-4 p-5 hover:shadow-lg"
                  >
                    <div className="flex flex-col gap-3 min-w-0">
                      <div className="flex items-center gap-3 flex-wrap">
                        <h3 className="font-mono text-base font-bold text-foreground truncate">
                          {a.tool}
                        </h3>
                        <Badge variant="outline-primary">
                          {a.agent_id}
                        </Badge>
                        <StatusBadge status="pending">
                          {t("approvals.status_pending")}
                        </StatusBadge>
                        <span className="ml-auto text-xs text-muted-foreground-subtle font-mono tabular-nums">
                          {relativeTime(a.created_at, locale)}
                        </span>
                      </div>

                      {Object.keys(a.arguments).length > 0 && (
                        <div className="rounded-lg neu-inset p-3">
                          <pre className="font-mono text-xs leading-relaxed text-foreground/80 line-clamp-4 whitespace-pre-wrap break-words">
                            {JSON.stringify(a.arguments, null, 2)}
                          </pre>
                        </div>
                      )}
                    </div>

                    <div className="grid grid-cols-2 md:flex md:items-center md:justify-end gap-2 border-t border-border/50 pt-3">
                      <Button
                        variant="outline-success"
                        size="sm"
                        onClick={() => handleResolve(a.id, "approved")}
                        disabled={processingIds.has(a.id)}
                        className="text-xs font-medium"
                      >
                        <Check className="h-4 w-4 mr-2" />
                        {t("approvals.approve")}
                      </Button>
                      <Button
                        variant="outline-destructive"
                        size="sm"
                        onClick={() => handleResolve(a.id, "rejected")}
                        disabled={processingIds.has(a.id)}
                        className="text-xs font-medium"
                      >
                        <X className="h-4 w-4 mr-2" />
                        {t("approvals.reject")}
                      </Button>
                    </div>
                  </Card>
                ))}
              </div>
            )}
          </div>
        </TabsContent>

        {/* Failures tab */}
        <TabsContent
          value="failures"
          forceMount
          className={activeTab !== "failures" ? "hidden" : "flex-1 overflow-y-auto"}
        >
          <div className="p-4 md:p-6 lg:p-8 selection:bg-primary/20">
            <SectionHeader
              icon={AlertTriangle}
              title={t("monitor.failures.title")}
              description={t("monitor.failures.subtitle", { total: String(failuresTotal) })}
              actions={
                <div className="flex items-center gap-2">
                  <Select value={failuresAgent} onValueChange={setFailuresAgent}>
                    <SelectTrigger className="h-9 w-44 text-xs">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {auditAgents.map((a) => (
                        <SelectItem key={a} value={a} className="text-xs">{a}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => failuresRefetch()}
                    disabled={failuresLoading}
                  >
                    <RefreshCw className={`mr-2 h-4 w-4 ${failuresLoading ? "animate-spin" : ""}`} />
                    {t("common.refresh")}
                  </Button>
                </div>
              }
            />

            {failures.length === 0 ? (
              <EmptyState icon={CheckCircle2} text={t("monitor.failures.empty")} />
            ) : (
              <Card className="overflow-hidden">
                <Table style={{ minWidth: 700 }}>
                  <TableHeader className="bg-muted/30">
                    <TableRow>
                      <TableHead>{t("monitor.failures.col_time")}</TableHead>
                      <TableHead>{t("monitor.failures.col_agent")}</TableHead>
                      <TableHead>{t("monitor.failures.col_kind")}</TableHead>
                      <TableHead>{t("monitor.failures.col_error")}</TableHead>
                      <TableHead>{t("monitor.failures.col_tool")}</TableHead>
                      <TableHead>{t("monitor.failures.col_provider")}</TableHead>
                      <TableHead className="text-right">{t("monitor.failures.col_iter")}</TableHead>
                      <TableHead className="text-right">{t("monitor.failures.col_dur")}</TableHead>
                      <TableHead>{t("monitor.failures.col_session")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {failures.map((f) => {
                      const expanded = failuresExpandedId === f.id;
                      return (
                        <Fragment key={f.id}>
                          <TableRow
                            className="cursor-pointer"
                            onClick={() => setFailuresExpandedId(expanded ? null : f.id)}
                          >
                            <TableCell className="whitespace-nowrap font-mono text-xs text-muted-foreground tabular-nums">
                              {relativeTime(f.failed_at, locale)}
                            </TableCell>
                            <TableCell className="font-mono text-xs">{f.agent_id}</TableCell>
                            <TableCell>
                              <Badge variant="outline" size="sm" className={failureKindClass(f.failure_kind)}>
                                {f.failure_kind}
                              </Badge>
                            </TableCell>
                            <TableCell className="max-w-md">
                              <span className="line-clamp-2 text-xs">{f.error_message}</span>
                            </TableCell>
                            <TableCell className="font-mono text-xs">{f.last_tool_name ?? "—"}</TableCell>
                            <TableCell className="font-mono text-xs">
                              {f.llm_provider ?? "—"}
                              {f.llm_model ? <span className="text-muted-foreground">/{f.llm_model}</span> : null}
                            </TableCell>
                            <TableCell className="text-right font-mono text-xs tabular-nums">
                              {f.iteration_count ?? "—"}
                            </TableCell>
                            <TableCell className="text-right font-mono text-xs tabular-nums">
                              {f.duration_secs != null ? formatDuration(f.duration_secs * 1000) : "—"}
                            </TableCell>
                            <TableCell>
                              <Button
                                variant="ghost"
                                size="xs"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  router.push(`/chat/?s=${f.session_id}`);
                                }}
                              >
                                {t("monitor.failures.open_session")}
                              </Button>
                            </TableCell>
                          </TableRow>
                          {expanded && (
                            <TableRow className="bg-muted/10 hover:bg-muted/10">
                              <TableCell colSpan={9} className="px-3 py-3">
                                <div className="grid gap-3 md:grid-cols-2">
                                  <div>
                                    <p className="text-xs font-medium text-muted-foreground mb-1">
                                      {t("monitor.failures.col_error")}
                                    </p>
                                    <pre className="font-mono text-xs whitespace-pre-wrap break-words bg-background/60 p-2 rounded border border-border/50">
                                      {f.error_message}
                                    </pre>
                                  </div>
                                  {f.last_tool_output && (
                                    <div>
                                      <p className="text-xs font-medium text-muted-foreground mb-1">
                                        {t("monitor.failures.last_output")}
                                      </p>
                                      <pre className="font-mono text-xs whitespace-pre-wrap break-words bg-background/60 p-2 rounded border border-border/50 max-h-48 overflow-y-auto">
                                        {f.last_tool_output}
                                      </pre>
                                    </div>
                                  )}
                                  {f.context && Object.keys(f.context).length > 0 && (
                                    <div className="md:col-span-2">
                                      <p className="text-xs font-medium text-muted-foreground mb-1">
                                        {t("monitor.failures.context")}
                                      </p>
                                      <pre className="font-mono text-xs whitespace-pre-wrap break-words bg-background/60 p-2 rounded border border-border/50">
                                        {JSON.stringify(f.context, null, 2)}
                                      </pre>
                                    </div>
                                  )}
                                </div>
                              </TableCell>
                            </TableRow>
                          )}
                        </Fragment>
                      );
                    })}
                  </TableBody>
                </Table>
              </Card>
            )}
          </div>
        </TabsContent>

        {/* Curator tab */}
        <TabsContent
          value="curator"
          forceMount
          className={activeTab !== "curator" ? "hidden" : "flex-1 overflow-y-auto p-4 md:p-6"}
        >
          <CuratorTab />
        </TabsContent>
      </Tabs>

      <ConfirmDialog
        open={restartConfirm !== null}
        onClose={() => setRestartConfirm(null)}
        onConfirm={confirmRestart}
        title={t("watchdog.restart_confirm_title")}
        description={t("watchdog.restart_confirm_body", { name: restartConfirm?.label ?? "" })}
        confirmLabel={t("watchdog.restart_service")}
        variant="warning"
      />
    </div>
  );
}

// ── Export with Suspense boundary for useSearchParams ───────────────────────

// ── CuratorTab component ────────────────────────────────────────────

function CuratorTab() {
  const { t, locale } = useTranslation();
  const { data, isLoading } = useCuratorRuns();
  const runs: CuratorRun[] = data?.runs ?? [];
  const [expandedId, setExpandedId] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="space-y-3">
        {[1, 2, 3].map((i) => (
          <Skeleton key={i} className="h-12 rounded-lg" />
        ))}
      </div>
    );
  }

  if (runs.length === 0) {
    return (
      <EmptyState
        icon={Sparkles}
        text={t("monitor.curator.empty")}
        hint={<p className="text-xs text-muted-foreground-subtle mt-1">{t("monitor.curator.no_runs")}</p>}
      />
    );
  }

  return (
    <Card className="overflow-hidden">
      <Table style={{ minWidth: 400 }}>
        <TableHeader className="bg-muted/30">
          <TableRow>
            <TableHead>{t("monitor.curator.col_time")}</TableHead>
            <TableHead>{t("monitor.curator.col_trigger")}</TableHead>
            <TableHead>{t("monitor.curator.col_status")}</TableHead>
            <TableHead className="text-right">{t("monitor.curator.col_phase1")}</TableHead>
            <TableHead className="text-right">{t("monitor.curator.col_phase2")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {runs.map((run) => {
            const isExpanded = expandedId === run.id;
            const isSkipped = !!run.skipped_reason;
            const isError = !!run.error;
            const statusColor = isError
              ? "text-destructive"
              : isSkipped
              ? "text-muted-foreground"
              : "text-success";
            const statusLabel = isError
              ? t("monitor.curator.status_error")
              : isSkipped
              ? t("monitor.curator.status_skipped")
              : t("monitor.curator.status_ok");
            const triggerLabel = run.triggered_by === "manual"
              ? t("monitor.curator.trigger_manual")
              : t("monitor.curator.trigger_scheduled");

            return (
              <Fragment key={run.id}>
                <TableRow
                  className="cursor-pointer"
                  onClick={() => setExpandedId(isExpanded ? null : run.id)}
                >
                  <TableCell className="text-xs text-muted-foreground font-mono truncate">
                    {relativeTime(run.started_at, locale)}
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant={run.triggered_by === "manual" ? "secondary" : "outline"}
                      size="sm"
                    >
                      {triggerLabel}
                    </Badge>
                  </TableCell>
                  <TableCell className={`text-xs font-medium ${statusColor}`}>{statusLabel}</TableCell>
                  <TableCell className="text-xs tabular-nums text-right">{run.phase1_transitions ?? "-"}</TableCell>
                  <TableCell className="text-xs tabular-nums text-right">{run.phase2_repairs ?? "-"}</TableCell>
                </TableRow>
                {isExpanded && (
                  <TableRow className="bg-muted/10 hover:bg-muted/10">
                    <TableCell colSpan={5} className="space-y-2 px-3 py-3">
                      {run.skipped_reason && (
                        <p className="text-xs text-muted-foreground">
                          <span className="font-medium">{t("monitor.curator.skipped")}</span> {run.skipped_reason}
                        </p>
                      )}
                      {run.error && (
                        <p className="text-xs text-destructive">
                          <span className="font-medium">{t("monitor.curator.error")}</span> {run.error}
                        </p>
                      )}
                      <div className="flex flex-wrap gap-4 text-xs text-muted-foreground">
                        <span>{t("monitor.curator.phase1_label")} <span className="font-medium text-foreground">{run.phase1_transitions}</span> {t("monitor.curator.transitions")}</span>
                        <span>{t("monitor.curator.phase2_label")} <span className="font-medium text-foreground">{run.phase2_repairs}</span> {t("monitor.curator.repairs")}</span>
                        <span>{t("monitor.curator.phase3_label")} <span className="font-medium text-foreground">{run.phase3_commands}</span> {t("monitor.curator.llm_commands")}</span>
                      </div>
                      {run.report_md && (
                        <pre className="text-xs font-mono whitespace-pre-wrap break-words bg-background/60 p-3 rounded border border-border/50 max-h-64 overflow-y-auto">
                          {run.report_md}
                        </pre>
                      )}
                    </TableCell>
                  </TableRow>
                )}
              </Fragment>
            );
          })}
        </TableBody>
      </Table>
    </Card>
  );
}

export default function MonitorPage() {
  return (
    <Suspense fallback={<div className="flex h-full items-center justify-center"><CircularLoader size="lg" /></div>}>
      <MonitorPageInner />
    </Suspense>
  );
}
