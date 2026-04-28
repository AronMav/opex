"use client";

import { Suspense, useEffect, useRef, useState, useCallback, memo, Fragment } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { apiGet, apiPost, apiPut } from "@/lib/api";
import { formatDuration, relativeTime } from "@/lib/format";
import { useAutoRefresh } from "@/hooks/use-auto-refresh";
import { useTranslation } from "@/hooks/use-translation";
import { useWsStore } from "@/stores/ws-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import { useUsage, useDailyUsage, useApprovals, useResolveApproval, useAudit, useSessionFailures } from "@/lib/queries";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { ErrorBanner } from "@/components/ui/error-banner";
import { EmptyState } from "@/components/ui/empty-state";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet, SheetContent, SheetHeader, SheetTitle, SheetDescription,
} from "@/components/ui/sheet";
import {
  Activity, Clock, Brain, Bot, User, Wrench, Zap, RefreshCw, Calendar, Database,
  CheckCircle2, XCircle, HeartPulse, AlertTriangle, Stethoscope,
  BarChart3, Cpu, ArrowUpRight, ArrowDownRight, DollarSign,
  ShieldCheck, Check, X,
  type LucideProps,
} from "lucide-react";
import type { StatusInfo, StatsInfo, UsageResponse, UsageSummary, DailyUsageResponse, AuditEvent, SessionFailureEntry } from "@/types/api";
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

const STATUS_VARIANT: Record<CheckStatus, "default" | "secondary" | "destructive"> = {
  ok: "default",
  warn: "secondary",
  error: "destructive",
};

const STATUS_LABEL: Record<CheckStatus, string> = {
  ok: "OK",
  warn: "WARN",
  error: "ERROR",
};

const CHECK_GROUPS: { titleKey: string; keys: string[] }[] = [
  { titleKey: "doctor.group_infrastructure", keys: ["database", "migrations", "pgvector", "memory_worker", "disk", "backup"] },
  { titleKey: "doctor.group_services", keys: ["toolgate", "browser_renderer", "searxng", "channels"] },
  { titleKey: "doctor.group_providers", keys: ["providers"] },
  { titleKey: "doctor.group_security", keys: ["security_audit", "secrets"] },
  { titleKey: "doctor.group_agents", keys: ["agents", "tool_health"] },
  { titleKey: "doctor.group_network", keys: ["network"] },
];

function FixHintButton({ hint }: { hint: string }) {
  const { t } = useTranslation();
  const router = useRouter();
  const route = getFixRoute(hint);

  if (route) {
    return (
      <div className="ml-[76px] mt-1 flex items-center gap-2">
        <span className="text-xs text-amber-600 dark:text-amber-400">{hint}</span>
        <Button
          variant="outline"
          size="sm"
          className="h-6 text-xs px-2"
          onClick={() => router.push(route)}
        >
          {t("doctor.fix")}
        </Button>
      </div>
    );
  }

  return (
    <div className="ml-[76px] mt-1">
      <span className="text-xs text-amber-600 dark:text-amber-400">{hint}</span>
    </div>
  );
}

function CheckRow({ name, result }: { name: string; result: CheckResult }) {
  const variant = STATUS_VARIANT[result.status] ?? "secondary";
  const label = STATUS_LABEL[result.status] ?? result.status.toUpperCase();
  const displayName = name.replace(/_/g, " ");

  return (
    <div className="flex flex-col gap-1 py-2 border-b last:border-0">
      <div className="flex items-center gap-3">
        <Badge variant={variant} className="w-16 justify-center text-xs shrink-0">
          {label}
        </Badge>
        <span className="font-medium text-sm capitalize">{displayName}</span>
        {result.latency_ms !== undefined && (
          <span className="ml-auto text-xs text-muted-foreground">{result.latency_ms}ms</span>
        )}
      </div>
      <p className="text-sm text-muted-foreground ml-[76px]">{result.message}</p>
      {result.fix_hint && (
        <FixHintButton hint={result.fix_hint} />
      )}
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
    <div className="rounded-lg border border-border bg-card text-card-foreground shadow-sm">
      <div className="px-4 py-3 border-b border-border">
        <h2 className="text-sm font-semibold text-foreground">{title}</h2>
      </div>
      <div className="px-4">
        {checks.map(({ name, result }) => (
          <CheckRow key={name} name={name} result={result} />
        ))}
      </div>
    </div>
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

interface ChannelInfo {
  id: string;
  agent_name: string;
  channel_type: string;
  display_name: string;
  status: string;
}

interface AlertingSettings {
  alert_channel_ids: string[];
  alert_events: string[];
}

const ALL_EVENTS = ["down", "restart", "recovery", "resource"] as const;
const EVENT_LABEL_KEYS: Record<string, string> = {
  down: "watchdog.event_down",
  restart: "watchdog.event_restart",
  recovery: "watchdog.event_recovery",
  resource: "watchdog.event_resource",
};

// ── MetricCard ──────────────────────────────────────────────────────────────

function MetricCard({ label, value, subValue, dot, valueClass, icon }: {
  label: string; value: string; subValue?: string;
  dot?: "success" | "error"; valueClass?: string; icon?: React.FC<LucideProps>;
}) {
  const Icon = icon;
  return (
    <div className="group neu-card neu-hover p-5 transition-all duration-300">
      <div className="flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            {Icon && <Icon className="text-primary/60 group-hover:text-primary transition-colors" size={16} />}
            <span className="text-xs font-semibold uppercase tracking-wide text-muted-foreground group-hover:text-foreground transition-colors">{label}</span>
          </div>
          {dot && <div className={`h-2 w-2 rounded-full ${dot === "success" ? "bg-success" : "bg-destructive"}`} />}
        </div>
        <div className="flex flex-col gap-1">
          <span className={`font-mono text-xl font-bold tracking-tight ${valueClass || "text-foreground"}`}>{value}</span>
          {subValue && <span className="text-xs text-muted-foreground truncate">{subValue}</span>}
        </div>
      </div>
    </div>
  );
}

// ── Statistics helpers ──────────────────────────────────────────────────────

const PERIOD_OPTIONS: { value: string; labelKey: TranslationKey }[] = [
  { value: "1", labelKey: "usage.period_24h" },
  { value: "7", labelKey: "usage.period_7d" },
  { value: "30", labelKey: "usage.period_30d" },
  { value: "90", labelKey: "usage.period_90d" },
];

const METRIC_COLORS = {
  messages: "text-blue-500",
  tokens: "text-emerald-500",
  cost: "text-amber-500",
  sessions: "text-purple-500",
} as const;

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
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

  const entries = Array.from(byDate.entries()).sort(([a], [b]) => a.localeCompare(b));
  const maxTokens = Math.max(1, ...entries.map(([, v]) => v.input + v.output));

  return (
    <div className="mb-8 rounded-xl border border-border bg-card/80 overflow-hidden">
      <div className="flex items-center gap-3 px-4 sm:px-5 py-4 border-b border-border/50 bg-muted/20">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary/10">
          <Calendar className="h-4 w-4 text-primary" />
        </div>
        <div>
          <h3 className="text-sm font-bold tracking-tight">{t("usage.tokens_by_day")}</h3>
          <span className="text-xs text-muted-foreground">
            <span className="inline-block w-2 h-2 rounded-sm bg-blue-500 mr-1" />{t("usage.input_legend")}
            <span className="inline-block w-2 h-2 rounded-sm bg-emerald-500 ml-3 mr-1" />{t("usage.output_legend")}
          </span>
        </div>
      </div>
      <div className="px-4 sm:px-5 py-4">
        <div className="flex items-end gap-[2px] sm:gap-1" style={{ height: 160 }}>
          {entries.map(([date, val], idx) => {
            const total = val.input + val.output;
            const pct = (total / maxTokens) * 100;
            const inputPct = total > 0 ? (val.input / total) * 100 : 50;
            const shortDate = date.slice(5);
            const labelStep = Math.max(1, Math.ceil(entries.length / 10));
            const showLabel = entries.length <= 14 || idx % labelStep === 0;

            return (
              <div
                key={date}
                className="group relative flex-1 min-w-0 flex flex-col justify-end h-full"
              >
                <div className="absolute bottom-full left-1/2 -translate-x-1/2 mb-2 hidden group-hover:block z-10">
                  <div className="rounded-lg border border-border bg-popover px-3 py-2 text-xs shadow-lg whitespace-nowrap">
                    <div className="font-bold mb-1">{date}</div>
                    <div className="text-blue-500">{formatTokens(val.input)} {t("usage.input_abbr")}</div>
                    <div className="text-emerald-500">{formatTokens(val.output)} {t("usage.output_abbr")}</div>
                    <div className="text-muted-foreground">{t("usage.calls", { count: val.calls })}</div>
                  </div>
                </div>
                <div
                  className="w-full rounded-t-sm overflow-hidden transition-all duration-300 group-hover:opacity-80 group-hover:ring-1 group-hover:ring-primary/20"
                  style={{ height: `${Math.max(pct, 2)}%` }}
                >
                  <div className="h-full flex flex-col">
                    <div className="bg-blue-500" style={{ flex: inputPct }} />
                    <div className="bg-emerald-500" style={{ flex: 100 - inputPct }} />
                  </div>
                </div>
                {showLabel ? (
                  <div className="text-[9px] text-muted-foreground/60 text-center mt-1 truncate">
                    {shortDate}
                  </div>
                ) : (
                  <div className="h-3" />
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
});

const SummaryCard = memo(function SummaryCard({
  icon: Icon,
  label,
  value,
  sub,
  accent,
  borderAccent,
  gradientFrom,
}: {
  icon: React.FC<{ className?: string }>;
  label: string;
  value: string;
  sub: string;
  accent: string;
  borderAccent?: string;
  gradientFrom?: string;
}) {
  return (
    <div className={`group relative rounded-xl border bg-gradient-to-br ${gradientFrom ?? ""} to-card/80 p-4 transition-all hover:shadow-sm overflow-hidden ${borderAccent ? `${borderAccent} hover:border-opacity-60` : "border-border hover:border-primary/20"}`}>
      <div className="absolute -right-3 -top-3 opacity-[0.04] group-hover:opacity-[0.08] transition-opacity">
        <Icon className="h-20 w-20" />
      </div>
      <div className="relative">
        <div className="flex items-center gap-2 mb-2">
          <Icon className={`h-4 w-4 ${accent}`} />
          <span className="text-xs font-medium text-muted-foreground uppercase tracking-wide">{label}</span>
        </div>
        <div className="text-2xl font-display font-bold tracking-tight">{value}</div>
        <div className="text-xs text-muted-foreground/60 mt-1">{sub}</div>
      </div>
    </div>
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
  sub_agent_timeout: "bg-orange-500/15 text-orange-600 border-orange-500/30",
  provider_error: "bg-destructive/15 text-destructive border-destructive/30",
  llm_error: "bg-destructive/15 text-destructive border-destructive/30",
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
  INFO: "text-primary/70",
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
  const [channels, setChannels] = useState<ChannelInfo[]>([]);
  const [alertSettings, setAlertSettings] = useState<AlertingSettings>({
    alert_channel_ids: [],
    alert_events: ["down", "restart", "recovery", "resource"],
  });
  const [alertDirty, setAlertDirty] = useState(false);
  const [alertSaving, setAlertSaving] = useState(false);
  const [alertOpen, setAlertOpen] = useState(false);

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

  const saveAlertSettings = async () => {
    setAlertSaving(true);
    try {
      await apiPut("/api/watchdog/settings", alertSettings);
      setAlertDirty(false);
    } catch (e) {
      setWdError(`Failed to save: ${e}`);
    }
    setAlertSaving(false);
  };

  const toggleChannel = (id: string) => {
    setAlertSettings((prev) => ({
      ...prev,
      alert_channel_ids: prev.alert_channel_ids.includes(id)
        ? prev.alert_channel_ids.filter((c) => c !== id)
        : [...prev.alert_channel_ids, id],
    }));
    setAlertDirty(true);
  };

  const toggleEvent = (event: string) => {
    setAlertSettings((prev) => ({
      ...prev,
      alert_events: prev.alert_events.includes(event)
        ? prev.alert_events.filter((e) => e !== event)
        : [...prev.alert_events, event],
    }));
    setAlertDirty(true);
  };

  const fetchWdData = useCallback(async (cancelled?: { current: boolean }) => {
    try {
      const [s, st, wd, chs, als] = await Promise.all([
        apiGet<StatusInfo>("/api/status"),
        apiGet<StatsInfo>("/api/stats"),
        apiGet<WatchdogStatus>("/api/watchdog/status").catch((e) => { console.warn("[watchdog] status fetch failed:", e); return null; }),
        apiGet<{ channels: ChannelInfo[] }>("/api/channels").catch((e) => { console.warn("[watchdog] channels fetch failed:", e); return { channels: [] }; }),
        apiGet<AlertingSettings>("/api/watchdog/settings").catch((e) => { console.warn("[watchdog] settings fetch failed:", e); return {
          alert_channel_ids: [] as string[],
          alert_events: ["down", "restart", "recovery", "resource"],
        }; }),
      ]);
      if (cancelled?.current) return;
      setWdStatus(s);
      setWdStats(st);
      if (wd && wd.checks) setWatchdog(wd);
      setChannels(chs.channels);
      if (!alertDirty) setAlertSettings(als);
      setLastFetch(new Date());
      setWdError("");
    } catch (e) {
      if (cancelled?.current) return;
      setWdError(`${e}`);
    }
  }, [alertDirty]);

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

  const auditParams: Record<string, string> = {
    limit: String(AUDIT_PAGE_SIZE),
    offset: String(auditOffset),
  };
  if (auditAgent !== "_all") auditParams.agent = auditAgent;
  if (auditEventType !== "_all") auditParams.event_type = auditEventType;

  const { data: auditEvents = [], isFetching: auditLoading, refetch: auditRefetch } = useAudit(auditParams);
  const auditHasMore = !auditSearch && auditEvents.length >= AUDIT_PAGE_SIZE;

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

  const filteredAudit = auditSearch
    ? auditEvents.filter((e: AuditEvent) => {
        const s = auditSearch.toLowerCase();
        return (
          e.event_type.toLowerCase().includes(s) ||
          e.agent_id.toLowerCase().includes(s) ||
          (e.actor && e.actor.toLowerCase().includes(s)) ||
          JSON.stringify(e.details).toLowerCase().includes(s)
        );
      })
    : auditEvents;

  // ── Failures state ────────────────────────────────────────────────────────

  const [failuresAgent, setFailuresAgent] = useState<string>("_all");
  const [failuresExpandedId, setFailuresExpandedId] = useState<string | null>(null);
  const failuresAgentParam = failuresAgent === "_all" ? null : failuresAgent;
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
      <Tabs value={activeTab} onValueChange={handleTabChange} className="flex flex-col flex-1 min-h-0">
        <div className="border-b border-border/50 bg-background px-4 md:px-6 pt-4 shrink-0">
          <TabsList className="h-9">
            <TabsTrigger value="watchdog" className="text-xs">{t("monitor.tab_watchdog")}</TabsTrigger>
            <TabsTrigger value="doctor" className="text-xs">{t("monitor.tab_doctor")}</TabsTrigger>
            <TabsTrigger value="logs" className="text-xs">{t("monitor.tab_logs")}</TabsTrigger>
            <TabsTrigger value="audit" className="text-xs">{t("monitor.tab_audit")}</TabsTrigger>
            <TabsTrigger value="statistics" className="text-xs">{t("monitor.tab_statistics")}</TabsTrigger>
            <TabsTrigger value="approvals" className="text-xs">{t("monitor.tab_approvals")}</TabsTrigger>
            <TabsTrigger value="failures" className="text-xs">{t("monitor.tab_failures")}</TabsTrigger>
          </TabsList>
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
              <div className="mb-6 flex flex-col md:flex-row md:items-center justify-between gap-4">
                <div className="flex flex-col gap-1">
                  <h2 className="font-display text-base font-bold tracking-tight flex items-center gap-2">
                    <HeartPulse className="h-5 w-5 text-primary" />
                    {t("watchdog.title")}
                  </h2>
                  <span className="text-sm text-muted-foreground">{t("watchdog.subtitle")}</span>
                </div>
                <div className="flex flex-wrap items-center gap-3">
                  {lastFetch && (
                    <span className="text-[10px] text-muted-foreground tabular-nums">
                      {lastFetch.toLocaleTimeString()}
                    </span>
                  )}
                  <Select value={String(refreshInterval)} onValueChange={(v) => setRefreshInterval(Number(v))}>
                    <SelectTrigger className="h-8 w-[80px] text-xs bg-card/50 border-border">
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
                    <div className="flex items-center gap-4 bg-muted/20 px-4 py-2 rounded-lg border border-border">
                      <div className="flex items-center gap-1.5">
                        <span className="text-[10px] text-muted-foreground">{t("watchdog.resource.disk")}</span>
                        <span className={`font-mono text-sm font-bold ${res.disk_critical ? "text-destructive" : res.disk_warning ? "text-warning" : "text-foreground"}`}>{res.disk_free_gb.toFixed(0)}GB</span>
                      </div>
                      <div className="w-px h-4 bg-border/50" />
                      <div className="flex items-center gap-1.5">
                        <span className="text-[10px] text-muted-foreground">{t("watchdog.resource.ram")}</span>
                        <span className={`font-mono text-sm font-bold ${res.ram_critical ? "text-destructive" : res.ram_warning ? "text-warning" : "text-foreground"}`}>{res.ram_used_percent.toFixed(0)}%</span>
                      </div>
                      {res.cpu_load_percent != null && (
                        <>
                          <div className="w-px h-4 bg-border/50" />
                          <div className="flex items-center gap-1.5">
                            <span className="text-[10px] text-muted-foreground">{t("watchdog.resource.cpu")}</span>
                            <span className={`font-mono text-sm font-bold ${res.cpu_load_percent > 90 ? "text-destructive" : res.cpu_load_percent > 70 ? "text-warning" : "text-foreground"}`}>{res.cpu_load_percent.toFixed(0)}%</span>
                          </div>
                        </>
                      )}
                    </div>
                  )}
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setAlertOpen(true)}
                  >
                    <span className="inline-block w-1.5 h-1.5 rounded-full bg-primary/60" />
                    {t("watchdog.alerting.title")}
                  </Button>
                </div>
              </div>

              {wdError && <ErrorBanner error={wdError} />}

              <div className="grid grid-cols-2 gap-3 md:gap-5 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
                <MetricCard
                  label={t("dashboard.status")}
                  value={wdChecks.length > 0 ? (allHealthy && (!watchdog?.containers || watchdog.containers.every(c => c.healthy)) ? "OK" : "ISSUES") : (s?.status?.toUpperCase() || "...")}
                  dot={wdChecks.length > 0 ? (allHealthy && (!watchdog?.containers || watchdog.containers.every(c => c.healthy)) ? "success" : "error") : (s?.status === "ok" ? "success" : "error")}
                  subValue={s?.version}
                  icon={Activity}
                />
                <MetricCard label={t("dashboard.uptime")} value={s ? formatDuration(s.uptime_seconds) : "..."} subValue={t("dashboard.uptime_sub")} icon={Clock} />
                <MetricCard label={t("dashboard.memory")} value={s?.memory_chunks?.toLocaleString() ?? "..."} subValue={t("dashboard.memory_sub")} icon={Brain} />
                <MetricCard label={t("dashboard.agents")} value={String(s?.agents?.length ?? "0")} subValue={s?.agents?.join(", ") || t("dashboard.agents_none")} icon={Bot} />
                <MetricCard label={t("dashboard.sessions")} value={String(s?.active_sessions ?? "0")} subValue={t("dashboard.sessions_sub")} icon={User} />
                <MetricCard label={t("dashboard.tools")} value={String(s?.tools_registered ?? "0")} subValue={t("dashboard.tools_sub")} icon={Wrench} />
                <MetricCard label={t("dashboard.messages_today")} value={String(st?.messages_today ?? "0")} subValue={t("dashboard.total_messages", { value: st?.total_messages.toLocaleString() ?? "0" })} icon={Zap} />
                <MetricCard label={t("dashboard.sessions_today")} value={String(st?.sessions_today ?? "0")} subValue={t("dashboard.total_sessions", { value: st?.total_sessions.toLocaleString() ?? "0" })} icon={RefreshCw} />
                <MetricCard label={t("dashboard.scheduled_jobs")} value={String(s?.scheduled_jobs ?? "0")} subValue={t("dashboard.scheduled_sub")} icon={Calendar} />
              </div>

              <Sheet open={alertOpen} onOpenChange={setAlertOpen}>
                <SheetContent side="right" className="w-80 sm:max-w-sm">
                  <SheetHeader>
                    <SheetTitle className="text-sm">{t("watchdog.alerting.title")}</SheetTitle>
                    <SheetDescription className="text-xs">
                      {t("watchdog.alerting.description")}
                    </SheetDescription>
                  </SheetHeader>

                  <div className="px-4 space-y-6 flex-1 overflow-y-auto">
                    <div className="space-y-3">
                      <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">{t("watchdog.alerting.channels")}</p>
                      {channels.length === 0 ? (
                        <p className="text-xs text-muted-foreground italic">{t("watchdog.alerting.no_channels")}</p>
                      ) : (
                        <div className="flex flex-col gap-1.5">
                          {channels.map((ch) => {
                            const selected = alertSettings.alert_channel_ids.includes(ch.id);
                            return (
                              <Button
                                key={ch.id}
                                variant={selected ? "default" : "outline"}
                                size="sm"
                                role="checkbox"
                                aria-checked={selected}
                                onClick={() => toggleChannel(ch.id)}
                                className="w-full justify-start text-xs h-auto py-2"
                              >
                                <span className="font-medium">{ch.agent_name}</span>
                                <span className="opacity-70"> / {ch.channel_type}</span>
                                {ch.display_name !== ch.channel_type && (
                                  <span className="opacity-50"> ({ch.display_name})</span>
                                )}
                              </Button>
                            );
                          })}
                        </div>
                      )}
                    </div>

                    <div className="space-y-3">
                      <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">{t("watchdog.alerting.events")}</p>
                      <div className="flex flex-col gap-1.5">
                        {ALL_EVENTS.map((event) => {
                          const selected = alertSettings.alert_events.includes(event);
                          return (
                            <Button
                              key={event}
                              variant={selected ? "default" : "outline"}
                              size="sm"
                              role="checkbox"
                              aria-checked={selected}
                              onClick={() => toggleEvent(event)}
                              className="w-full justify-start text-xs"
                            >
                              {EVENT_LABEL_KEYS[event] ? t(EVENT_LABEL_KEYS[event] as Parameters<typeof t>[0]) : event}
                            </Button>
                          );
                        })}
                      </div>
                    </div>
                  </div>

                  {alertDirty && (
                    <div className="px-4 pb-4">
                      <Button
                        onClick={saveAlertSettings}
                        disabled={alertSaving}
                        className="w-full"
                      >
                        {alertSaving ? t("common.saving") : t("common.save")}
                      </Button>
                    </div>
                  )}
                </SheetContent>
              </Sheet>

              {wdChecks.length > 0 && (
                <div className="mt-8">
                  <div className="flex items-center gap-3 mb-4">
                    <HeartPulse size={16} className="text-primary/60" />
                    <span className="text-sm font-semibold text-foreground">{t("watchdog.services")}</span>
                    <Badge variant="outline" className="text-[10px] font-mono">
                      {wdChecks.filter(([,v]) => v.ok).length}/{wdChecks.length}
                    </Badge>
                  </div>
                  <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-3">
                    {wdChecks.map(([name, svc]) => (
                      <div
                        key={name}
                        className={`neu-card p-3 flex flex-col gap-1.5 ${
                          svc.flapping ? "border-l-[3px] border-l-warning" : !svc.ok ? "border-l-[3px] border-l-destructive" : ""
                        }`}
                      >
                        <div className="flex items-center justify-between">
                          <span className="text-xs font-semibold text-muted-foreground">{name}</span>
                          <div className="flex items-center gap-1">
                            {svc.can_restart && (
                              <Button
                                variant="ghost"
                                size="icon"
                                onClick={() => restartService(name)}
                                disabled={restarting === name}
                                aria-label={t("watchdog.restart_service")}
                                className="min-h-[44px] min-w-[44px]"
                              >
                                <RefreshCw className={`h-3 w-3 text-muted-foreground ${restarting === name ? "animate-spin" : ""}`} />
                              </Button>
                            )}
                            {svc.ok ? (
                              <CheckCircle2 size={14} className="text-success" />
                            ) : svc.flapping ? (
                              <AlertTriangle size={14} className="text-warning" />
                            ) : (
                              <XCircle size={14} className="text-destructive" />
                            )}
                          </div>
                        </div>
                        <span className="font-mono text-xs text-foreground/60">{svc.latency_ms}ms</span>
                        {svc.error && (
                          <span className="text-[10px] text-destructive truncate" title={svc.error}>{svc.error}</span>
                        )}
                        {svc.flapping && (
                          <Badge variant="outline" className="text-[9px] w-fit border-warning/30 text-warning">{t("watchdog.flapping")}</Badge>
                        )}
                        {svc.last_restart && (
                          <Badge variant="outline" className="text-[9px] w-fit border-warning/30 text-warning">{t("watchdog.restarted")}</Badge>
                        )}
                      </div>
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
                          <Database size={16} className="text-primary/60" />
                          <span className="text-sm font-semibold text-foreground">{t("watchdog.infrastructure")}</span>
                          <Badge variant="outline" className="text-[10px] font-mono">
                            {infra.filter(c => c.healthy).length}/{infra.length}
                          </Badge>
                        </div>
                        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-3">
                          {infra.map((c) => (
                            <div key={c.name} className={`neu-card px-3 py-2.5 flex items-center gap-2 group ${!c.healthy ? "border-l-[3px] border-l-destructive bg-destructive/5" : ""}`}>
                              <span className={`h-2 w-2 rounded-full shrink-0 ${c.healthy ? "bg-success" : "bg-destructive"}`} />
                              <div className="min-w-0 flex-1">
                                <span className="text-xs font-semibold text-foreground block">{c.name}</span>
                                <span className={`text-[10px] block ${c.healthy ? "text-muted-foreground" : "text-destructive"}`}>{c.status}</span>
                              </div>
                              <Button
                                variant="ghost"
                                size="icon"
                                onClick={() => restartContainer(c.docker_name)}
                                disabled={restarting === c.docker_name}
                                aria-label={t("watchdog.restart_service")}
                                className="min-h-[44px] min-w-[44px] shrink-0"
                              >
                                <RefreshCw className={`h-3 w-3 text-muted-foreground ${restarting === c.docker_name ? "animate-spin" : ""}`} />
                              </Button>
                            </div>
                          ))}
                        </div>
                      </div>
                    )}
                    {agents.length > 0 && (
                      <div>
                        <div className="flex items-center gap-3 mb-3">
                          <Bot size={16} className="text-primary/60" />
                          <span className="text-sm font-semibold text-foreground">{t("watchdog.agents")}</span>
                          <Badge variant="outline" className="text-[10px] font-mono">
                            {agents.filter(c => c.healthy).length}/{agents.length}
                          </Badge>
                        </div>
                        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-3">
                          {agents.map((c) => (
                            <div key={c.name} className={`neu-card px-3 py-2.5 flex items-center gap-2 group ${!c.healthy ? "border-l-[3px] border-l-destructive bg-destructive/5" : ""}`}>
                              <span className={`h-2 w-2 rounded-full shrink-0 ${c.healthy ? "bg-success" : "bg-destructive"}`} />
                              <div className="min-w-0 flex-1">
                                <span className="text-xs font-semibold text-foreground block">{c.name}</span>
                                <span className={`text-[10px] block ${c.healthy ? "text-muted-foreground" : "text-destructive"}`}>{c.status}</span>
                              </div>
                              <Button
                                variant="ghost"
                                size="icon"
                                onClick={() => restartContainer(c.docker_name)}
                                disabled={restarting === c.docker_name}
                                aria-label={t("watchdog.restart_service")}
                                className="min-h-[44px] min-w-[44px] shrink-0"
                              >
                                <RefreshCw className={`h-3 w-3 text-muted-foreground ${restarting === c.docker_name ? "animate-spin" : ""}`} />
                              </Button>
                            </div>
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
              <div className="flex items-center justify-between mb-4">
                <div className="flex items-center gap-3">
                  <Stethoscope className="text-primary" size={20} />
                  <div>
                    <h2 className="text-base font-bold">{t("doctor.title")}</h2>
                    <p className="text-xs text-muted-foreground">{t("doctor.subtitle")}</p>
                  </div>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => doctorRefetch()}
                  disabled={doctorFetching}
                >
                  <RefreshCw size={14} className={doctorFetching ? "animate-spin mr-2" : "mr-2"} />
                  {t("common.refresh")}
                </Button>
              </div>

              {doctorData && (
                <div
                  className={`rounded-md p-3 text-sm font-medium mb-4 ${
                    doctorData.ok
                      ? "bg-green-50 text-green-800 dark:bg-green-950 dark:text-green-200"
                      : "bg-red-50 text-red-800 dark:bg-red-950 dark:text-red-200"
                  }`}
                >
                  {doctorData.ok ? t("doctor.all_ok") : t("doctor.needs_attention")}
                </div>
              )}

              {doctorLoading && (
                <p className="text-muted-foreground text-sm">{t("doctor.loading")}</p>
              )}

              {doctorError && (
                <div className="rounded-md border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive mb-4">
                  {t("doctor.error")}
                </div>
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
                  <SelectTrigger className="h-9 min-w-[80px] sm:min-w-[110px] border-border bg-card/50 font-mono text-sm rounded-lg">
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

              <div className="flex items-center justify-between md:justify-end gap-4 w-full md:w-auto mt-2 md:mt-0">
                <div className="flex sm:hidden items-center gap-2">
                  <Switch checked={autoScroll} onCheckedChange={setAutoScroll} className="scale-75 data-[state=checked]:bg-primary" />
                  <span className="text-xs text-muted-foreground">{t("logs.autoscroll_short")}</span>
                </div>
                <div className="flex items-center gap-4">
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
                      a.download = `hydeclaw-logs-${new Date().toISOString().slice(0,10)}.txt`;
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
                        <span className="w-16 text-muted-foreground/60 tabular-nums group-hover:text-muted-foreground/70 transition-colors">
                          {new Date(l.timestamp).toLocaleTimeString(locale === "en" ? "en-US" : "ru-RU", { hour12: false })}
                        </span>
                        <span className={`w-12 font-bold uppercase tracking-tighter ${LEVEL_COLORS[l.level] || ""}`}>
                          {l.level}
                        </span>
                        {l.target && (
                          <span className="w-24 md:w-32 truncate text-primary/60 font-bold hidden sm:inline-block" title={l.target}>
                            [{l.target}]
                          </span>
                        )}
                      </div>
                      {l.target && (
                        <span className="text-primary/60 font-bold sm:hidden" title={l.target}>
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
                  <SelectTrigger className="h-9 min-w-[90px] sm:min-w-[120px] border-border bg-card/50 text-sm rounded-lg">
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
                  <SelectTrigger className="h-9 min-w-[100px] sm:min-w-[150px] border-border bg-card/50 text-sm rounded-lg">
                    <SelectValue placeholder={t("audit.event_type_placeholder")} />
                  </SelectTrigger>
                  <SelectContent className="border-border rounded-lg">
                    {EVENT_TYPES.map((et) => (
                      <SelectItem key={et.value} value={et.value} className="text-sm">{t(et.labelKey)}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>

                <Input
                  placeholder={t("audit.search_placeholder")}
                  value={auditSearch}
                  onChange={(e) => setAuditSearch(e.target.value)}
                  className="h-9 flex-1 md:w-48 md:flex-none border-border bg-card/50 text-sm placeholder:text-muted-foreground/60 rounded-lg focus:ring-primary/20"
                />
              </div>

              <div className="flex items-center gap-4 shrink-0">
                <span className="font-mono text-xs tabular-nums text-muted-foreground hidden md:inline">
                  {t("audit.events_count", { count: filteredAudit.length })}
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
                  <p className="text-sm text-muted-foreground animate-pulse">{t("common.loading")}</p>
                </div>
              ) : filteredAudit.length === 0 ? (
                <div className="flex h-full flex-col items-center justify-center gap-4 opacity-40">
                  <div className="h-16 w-px bg-gradient-to-b from-transparent via-primary/50 to-transparent" />
                  <p className="text-sm text-muted-foreground">{t("audit.no_events")}</p>
                </div>
              ) : (
                <div className="flex flex-col gap-2">
                  {filteredAudit.map((e: AuditEvent) => (
                    <div
                      key={e.id}
                      className="group rounded-lg border border-border/50 bg-card/30 transition-colors hover:bg-card/60"
                    >
                      <button
                        type="button"
                        className="flex w-full items-center gap-3 px-4 py-3 text-left"
                        onClick={() => setExpandedId(expandedId === e.id ? null : e.id)}
                      >
                        <span className="shrink-0 w-20 font-mono text-xs tabular-nums text-muted-foreground/70">
                          {new Date(e.created_at).toLocaleTimeString(locale === "en" ? "en-US" : "ru-RU", { hour12: false })}
                        </span>
                        <span className="shrink-0 w-20 font-mono text-xs text-muted-foreground truncate" title={e.agent_id}>
                          {e.agent_id}
                        </span>
                        <span className={`shrink-0 rounded-md px-2 py-0.5 text-xs font-medium ${EVENT_COLORS[e.event_type] || "bg-muted text-muted-foreground"}`}>
                          {e.event_type}
                        </span>
                        {e.actor && (
                          <span className="text-xs text-muted-foreground/60 truncate">
                            {t("audit.from", { actor: e.actor })}
                          </span>
                        )}
                        <span className="ml-auto text-xs text-muted-foreground/40">
                          {new Date(e.created_at).toLocaleDateString(locale === "en" ? "en-US" : "ru-RU")}
                        </span>
                        <span className="text-muted-foreground/40 transition-transform" style={{ transform: expandedId === e.id ? "rotate(90deg)" : "rotate(0)" }}>
                          ▶
                        </span>
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
                  {auditOffset + 1}–{auditOffset + filteredAudit.length}
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
                  <div key={i} className="h-32 rounded-xl border border-border bg-muted/20 animate-pulse" />
                ))}
              </div>
            ) : (
              <>
                <div className="mb-8 md:mb-10 flex flex-col md:flex-row md:items-center justify-between gap-4">
                  <div className="flex flex-col gap-1">
                    <h2 className="font-display text-lg font-bold tracking-tight">{t("usage.title")}</h2>
                    <span className="text-sm text-muted-foreground">{t("usage.subtitle")}</span>
                  </div>
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
                </div>

                {statsError && <ErrorBanner error={statsError} />}

                {totalTokens > 0 && (
                  <div className="mb-5 rounded-xl border border-amber-500/20 bg-gradient-to-r from-amber-500/5 via-card/80 to-amber-500/5 p-5 flex items-center justify-between gap-4 overflow-hidden relative">
                    <div className="absolute -right-6 -top-6 opacity-[0.04]">
                      <Zap className="h-32 w-32 text-amber-500" />
                    </div>
                    <div className="relative">
                      <div className="flex items-center gap-2 mb-1">
                        <Zap className="h-4 w-4 text-amber-500" />
                        <span className="text-xs font-medium text-muted-foreground uppercase tracking-wide">{t("usage.total_tokens_summary")}</span>
                      </div>
                      <div className="text-4xl font-display font-bold tracking-tight text-amber-500">
                        {formatTokens(totalTokens)}
                      </div>
                      <div className="text-xs text-muted-foreground/60 mt-1">
                        {t("usage.period_days", { days: usageData?.days ?? 0 })} &middot; {totalCalls.toLocaleString()} {t("usage.calls_short")}
                      </div>
                    </div>
                    <div className="relative flex flex-col items-end gap-1 shrink-0">
                      <div className="text-right">
                        <div className="text-xs text-muted-foreground/60">{t("usage.input_short")}</div>
                        <div className="text-sm font-mono font-bold text-blue-500">{formatTokens(totalInput)}</div>
                      </div>
                      <div className="text-right">
                        <div className="text-xs text-muted-foreground/60">{t("usage.output_short")}</div>
                        <div className="text-sm font-mono font-bold text-emerald-500">{formatTokens(totalOutput)}</div>
                      </div>
                      {totalCost > 0 && (
                        <div className="text-right">
                          <div className="text-xs text-muted-foreground/60">{t("usage.estimated_cost")}</div>
                          <div className="text-sm font-mono font-bold text-purple-500">${totalCost.toFixed(4)}</div>
                        </div>
                      )}
                    </div>
                  </div>
                )}

                <div className="grid grid-cols-2 md:grid-cols-4 gap-3 mb-8">
                  <SummaryCard
                    icon={Zap}
                    label={t("usage.total_tokens")}
                    value={formatTokens(totalTokens)}
                    sub={t("usage.period_days", { days: usageData?.days ?? 0 })}
                    accent={METRIC_COLORS.cost}
                    borderAccent="border-amber-500/30"
                    gradientFrom="from-amber-500/5"
                  />
                  <SummaryCard
                    icon={ArrowUpRight}
                    label={t("usage.input_tokens")}
                    value={formatTokens(totalInput)}
                    sub={t("usage.pct_of_total", { pct: ((totalInput / Math.max(totalTokens, 1)) * 100).toFixed(0) })}
                    accent={METRIC_COLORS.messages}
                    borderAccent="border-blue-500/30"
                    gradientFrom="from-blue-500/5"
                  />
                  <SummaryCard
                    icon={ArrowDownRight}
                    label={t("usage.output_tokens")}
                    value={formatTokens(totalOutput)}
                    sub={t("usage.pct_of_total", { pct: ((totalOutput / Math.max(totalTokens, 1)) * 100).toFixed(0) })}
                    accent={METRIC_COLORS.tokens}
                    borderAccent="border-emerald-500/30"
                    gradientFrom="from-emerald-500/5"
                  />
                  <SummaryCard
                    icon={DollarSign}
                    label={t("usage.estimated_cost")}
                    value={totalCost > 0 ? `$${totalCost.toFixed(4)}` : "$0"}
                    sub={t("usage.api_calls", { count: totalCalls.toLocaleString() })}
                    accent={METRIC_COLORS.sessions}
                    borderAccent="border-purple-500/30"
                    gradientFrom="from-purple-500/5"
                  />
                </div>

                {dailyData && dailyData.daily.length > 0 && <DailyChart data={dailyData.daily} />}

                {usage.length === 0 ? (
                  <div className="flex flex-col items-center justify-center py-20 text-muted-foreground">
                    <BarChart3 className="h-12 w-12 mb-3 opacity-30" />
                    <p className="text-sm font-medium">{t("usage.no_data")}</p>
                    <p className="text-xs mt-1 opacity-60">{t("usage.tracking_hint")}</p>
                  </div>
                ) : (
                  <div className="space-y-6">
                    {Array.from(byAgent.entries()).map(([agent, rows]) => {
                      const agentInput = rows.reduce((s, r) => s + r.total_input, 0);
                      const agentOutput = rows.reduce((s, r) => s + r.total_output, 0);
                      const agentCalls = rows.reduce((s, r) => s + r.call_count, 0);

                      return (
                        <div key={agent} className="rounded-xl border border-border bg-card/80 overflow-hidden">
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
                            <div className="flex gap-4 sm:gap-6 text-right ml-11 sm:ml-0">
                              <div>
                                <div className="text-xs text-muted-foreground">{t("usage.input_short")}</div>
                                <div className="text-sm font-mono font-bold text-blue-500">{formatTokens(agentInput)}</div>
                              </div>
                              <div>
                                <div className="text-xs text-muted-foreground">{t("usage.output_short")}</div>
                                <div className="text-sm font-mono font-bold text-emerald-500">{formatTokens(agentOutput)}</div>
                              </div>
                              <div>
                                <div className="text-xs text-muted-foreground">{t("usage.total_short")}</div>
                                <div className="text-sm font-mono font-bold">{formatTokens(agentInput + agentOutput)}</div>
                              </div>
                            </div>
                          </div>

                          <div className="divide-y divide-border/30">
                            {rows.map((row, rowIdx) => {
                              const rowTotal = row.total_input + row.total_output;
                              const pct = (rowTotal / maxTotal) * 100;

                              return (
                                <div
                                  key={`${row.agent_id}-${row.provider}-${row.model}`}
                                  className={`relative px-4 sm:px-5 py-3 group hover:bg-muted/20 transition-colors ${
                                    rowIdx % 2 === 1 ? "bg-muted/[0.04]" : ""
                                  }`}
                                >
                                  <div
                                    className="absolute inset-y-0 left-0 bg-primary/[0.04] transition-all duration-500"
                                    style={{ width: `${pct}%` }}
                                  />
                                  <div className="relative flex flex-col sm:flex-row sm:items-center justify-between gap-1.5 sm:gap-3">
                                    <div className="flex items-center gap-3 flex-wrap">
                                      <span className="inline-flex h-6 items-center rounded-md bg-muted/60 px-2 text-xs font-mono font-medium text-muted-foreground">
                                        {row.provider}
                                      </span>
                                      {row.model && (
                                        <span className="inline-flex h-6 items-center rounded-md bg-primary/10 px-2 text-xs font-mono font-medium text-primary/80">
                                          {row.model}
                                        </span>
                                      )}
                                      <span className="text-xs text-muted-foreground/60">
                                        {t("usage.calls", { count: row.call_count.toLocaleString() })}
                                      </span>
                                    </div>
                                    <div className="flex gap-3 sm:gap-5 text-right ml-0 sm:ml-auto">
                                      <span className="text-xs font-mono tabular-nums text-blue-500/80">
                                        {formatTokens(row.total_input)} {t("usage.input_abbr")}
                                      </span>
                                      <span className="text-xs font-mono tabular-nums text-emerald-500/80">
                                        {formatTokens(row.total_output)} {t("usage.output_abbr")}
                                      </span>
                                      <span className="text-xs font-mono font-bold tabular-nums">
                                        {formatTokens(rowTotal)}
                                      </span>
                                      {row.estimated_cost != null && (
                                        <span className="text-xs font-mono tabular-nums text-purple-500/80">
                                          ${row.estimated_cost.toFixed(4)}
                                        </span>
                                      )}
                                    </div>
                                  </div>
                                </div>
                              );
                            })}
                          </div>
                        </div>
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
            <div className="mb-8 flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
              <div>
                <h2 className="font-display text-lg font-bold tracking-tight text-foreground">
                  {t("approvals.title")}
                </h2>
                <p className="text-sm text-muted-foreground mt-1">
                  {t("approvals.subtitle")}
                </p>
              </div>
              <div className="flex gap-2">
                <Button variant="outline" size="sm" onClick={() => approvalsRefetch()} disabled={approvalsLoading}>
                  <RefreshCw className={`mr-2 h-4 w-4 ${approvalsLoading ? "animate-spin" : ""}`} />
                  {t("common.refresh")}
                </Button>
              </div>
            </div>

            {approvalsErrorMessage && <ErrorBanner error={approvalsErrorMessage} />}

            {pending.length === 0 ? (
              <EmptyState icon={ShieldCheck} text={t("approvals.no_pending")} />
            ) : (
              <div className="grid gap-4 md:gap-6">
                {pending.map((a) => (
                  <div
                    key={a.id}
                    className="group relative flex flex-col gap-4 neu-card p-5 transition-all duration-300 hover:shadow-lg"
                  >
                    <div className="flex flex-col gap-3 min-w-0">
                      <div className="flex items-center gap-3 flex-wrap">
                        <h3 className="font-mono text-base font-bold text-foreground truncate">
                          {a.tool}
                        </h3>
                        <Badge
                          variant="outline"
                          className="text-xs border-primary/40 text-primary bg-primary/5"
                        >
                          {a.agent_id}
                        </Badge>
                        <Badge
                          variant="secondary"
                          className="text-xs bg-warning/20 text-warning border-warning/30"
                        >
                          {t("approvals.status_pending")}
                        </Badge>
                        <span className="ml-auto text-xs text-muted-foreground/60 font-mono tabular-nums">
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
                        variant="outline"
                        size="sm"
                        onClick={() => handleResolve(a.id, "approved")}
                        disabled={processingIds.has(a.id)}
                        className="text-xs font-medium border-success/50 text-success hover:bg-success/10"
                      >
                        <Check className="h-3 w-3 mr-2" />
                        {t("approvals.approve")}
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => handleResolve(a.id, "rejected")}
                        disabled={processingIds.has(a.id)}
                        className="text-xs font-medium border-destructive/50 text-destructive hover:bg-destructive/10"
                      >
                        <X className="h-3 w-3 mr-2" />
                        {t("approvals.reject")}
                      </Button>
                    </div>
                  </div>
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
            <div className="mb-6 flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
              <div>
                <h2 className="font-display text-lg font-bold tracking-tight text-foreground flex items-center gap-2">
                  <AlertTriangle className="h-5 w-5 text-warning" />
                  {t("monitor.failures.title")}
                </h2>
                <p className="text-sm text-muted-foreground mt-1">
                  {t("monitor.failures.subtitle", { total: String(failuresTotal) })}
                </p>
              </div>
              <div className="flex items-center gap-2">
                <Select value={failuresAgent} onValueChange={setFailuresAgent}>
                  <SelectTrigger className="h-8 w-[180px] text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="_all" className="text-xs">{t("monitor.failures.agent_all")}</SelectItem>
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
            </div>

            {failures.length === 0 ? (
              <EmptyState icon={CheckCircle2} text={t("monitor.failures.empty")} />
            ) : (
              <div className="rounded-lg border border-border bg-card overflow-x-auto">
                <table className="w-full text-sm">
                  <thead className="bg-muted/30 text-xs text-muted-foreground">
                    <tr>
                      <th className="text-left px-3 py-2 font-medium">{t("monitor.failures.col_time")}</th>
                      <th className="text-left px-3 py-2 font-medium">{t("monitor.failures.col_agent")}</th>
                      <th className="text-left px-3 py-2 font-medium">{t("monitor.failures.col_kind")}</th>
                      <th className="text-left px-3 py-2 font-medium">{t("monitor.failures.col_error")}</th>
                      <th className="text-left px-3 py-2 font-medium">{t("monitor.failures.col_tool")}</th>
                      <th className="text-left px-3 py-2 font-medium">{t("monitor.failures.col_provider")}</th>
                      <th className="text-right px-3 py-2 font-medium">{t("monitor.failures.col_iter")}</th>
                      <th className="text-right px-3 py-2 font-medium">{t("monitor.failures.col_dur")}</th>
                      <th className="text-left px-3 py-2 font-medium">{t("monitor.failures.col_session")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {failures.map((f) => {
                      const expanded = failuresExpandedId === f.id;
                      return (
                        <Fragment key={f.id}>
                          <tr
                            className="border-t border-border/50 hover:bg-muted/20 cursor-pointer"
                            onClick={() => setFailuresExpandedId(expanded ? null : f.id)}
                          >
                            <td className="px-3 py-2 whitespace-nowrap font-mono text-xs text-muted-foreground tabular-nums">
                              {relativeTime(f.failed_at, locale)}
                            </td>
                            <td className="px-3 py-2 font-mono text-xs">{f.agent_id}</td>
                            <td className="px-3 py-2">
                              <Badge variant="outline" className={`text-[10px] ${failureKindClass(f.failure_kind)}`}>
                                {f.failure_kind}
                              </Badge>
                            </td>
                            <td className="px-3 py-2 max-w-md">
                              <span className="line-clamp-2 text-xs">{f.error_message}</span>
                            </td>
                            <td className="px-3 py-2 font-mono text-xs">{f.last_tool_name ?? "—"}</td>
                            <td className="px-3 py-2 font-mono text-xs">
                              {f.llm_provider ?? "—"}
                              {f.llm_model ? <span className="text-muted-foreground">/{f.llm_model}</span> : null}
                            </td>
                            <td className="px-3 py-2 text-right font-mono text-xs tabular-nums">
                              {f.iteration_count ?? "—"}
                            </td>
                            <td className="px-3 py-2 text-right font-mono text-xs tabular-nums">
                              {f.duration_secs != null ? formatDuration(f.duration_secs * 1000) : "—"}
                            </td>
                            <td className="px-3 py-2">
                              <Button
                                variant="ghost"
                                size="sm"
                                className="h-6 px-2 text-xs"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  router.push(`/chat/?s=${f.session_id}`);
                                }}
                              >
                                {t("monitor.failures.open_session")}
                              </Button>
                            </td>
                          </tr>
                          {expanded && (
                            <tr className="bg-muted/10">
                              <td colSpan={9} className="px-3 py-3">
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
                              </td>
                            </tr>
                          )}
                        </Fragment>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        </TabsContent>
      </Tabs>
    </div>
  );
}

// ── Export with Suspense boundary for useSearchParams ───────────────────────

export default function MonitorPage() {
  return (
    <Suspense fallback={<div className="flex h-full items-center justify-center"><span className="text-sm text-muted-foreground">Loading...</span></div>}>
      <MonitorPageInner />
    </Suspense>
  );
}
