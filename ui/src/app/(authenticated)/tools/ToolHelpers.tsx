"use client";

import type { ReactNode } from "react";
import { useTranslation } from "@/hooks/use-translation";

/* ── Sub-components ──────────────────────────────────────────────── */

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5">
      <label className="text-xs font-medium text-foreground">{label}</label>
      {hint && <span className="text-2xs text-muted-foreground -mt-1">{hint}</span>}
      {children}
    </div>
  );
}

export function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between items-center bg-muted/20 rounded px-2.5 py-1.5 border border-border/50">
      <span className="text-muted-foreground shrink-0">{label}</span>
      <span className="font-mono text-foreground/80 truncate ml-2 min-w-0">{value}</span>
    </div>
  );
}

export function TypeBadge({ type }: { type: string }) {
  const colors: Record<string, string> = {
    GET: "border-success/30 bg-success/10 text-success",
    POST: "border-primary/30 bg-primary/10 text-primary",
    PUT: "border-warning/30 bg-warning/10 text-warning",
    PATCH: "border-chart-7/30 bg-chart-7/10 text-chart-7",
    DELETE: "border-destructive/30 bg-destructive/10 text-destructive",
    MCP: "border-chart-5/30 bg-chart-5/10 text-chart-5",
    INT: "border-chart-6/30 bg-chart-6/10 text-chart-6",
    EXT: "border-chart-3/30 bg-chart-3/10 text-chart-3",
  };
  const cls = colors[type] ?? "border-border bg-muted/30 text-muted-foreground";
  return (
    <span className={`rounded-full border px-2 py-0.5 text-3xs font-bold tracking-wide shrink-0 ${cls}`}>
      {type}
    </span>
  );
}

export function StatusBadge({ status }: { status: string }) {
  const { t } = useTranslation();
  const map: Record<string, { dot: string; bg: string; text: string; labelKey: "tools.verified" | "tools.draft" }> = {
    verified: { dot: "bg-success", bg: "border-success/30 bg-success/10", text: "text-success", labelKey: "tools.verified" },
    draft: { dot: "bg-warning animate-pulse", bg: "border-warning/30 bg-warning/10", text: "text-warning", labelKey: "tools.draft" },
  };
  const entry = map[status];
  const s = entry
    ? { ...entry, label: t(entry.labelKey) }
    : { dot: "bg-muted-foreground-subtle", bg: "border-border bg-muted/30", text: "text-muted-foreground", label: t("tools.disabled") };
  return (
    <span className={`flex items-center gap-1 rounded-full border px-2 py-0.5 text-3xs font-medium shrink-0 ${s.bg} ${s.text}`}>
      <span className={`h-1.5 w-1.5 rounded-full ${s.dot}`} /> {s.label}
    </span>
  );
}
