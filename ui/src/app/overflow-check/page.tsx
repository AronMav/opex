"use client";

import { Tabs } from "@/components/ui/tabs";
import { FilterTabsList } from "@/components/ui/filter-tabs";
import {
  Globe, Activity, Stethoscope, ScrollText, Shield, BarChart3, CheckCheck, AlertTriangle,
} from "lucide-react";

// Test-only harness for the overflow guard (src/__e2e__/overflow.spec.ts).
//
// The guard asserts `[data-overflow-check]` elements never scroll horizontally
// (scrollWidth <= clientWidth). It targets the GLOBAL wrapping baseline (Layer 1
// of the overflow-prevention spec): a bare `.prose` / `.font-mono` element that
// holds a long unbroken token. Without the globals rule these do NOT wrap
// (`overflow-wrap: normal`) and overflow their fixed-width box → the guard fails.
// With the rule they wrap → pass. This protects every prose/mono site app-wide.
//
// The tab bar + provider row below are rendered for visual/screenshot review
// only (their fixes are component-level: scroll affordance / min-w-0); they are
// NOT under data-overflow-check because a tab list legitimately scrolls.

const LONG_URL =
  "https://www.youtube.com/watch?v=VKfYTaepfusVKfYTaepfusVKfYTaepfusVKfYTaepfus&feature=youtu.be&list=PLxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

const TABS = [
  { value: "watchdog", label: "Watchdog", icon: <Shield /> },
  { value: "diag", label: "Диагностика", icon: <Stethoscope /> },
  { value: "logs", label: "Логи", icon: <ScrollText /> },
  { value: "audit", label: "Аудит", icon: <Activity /> },
  { value: "stats", label: "Статистика", icon: <BarChart3 /> },
  { value: "approvals", label: "Одобрения", icon: <CheckCheck /> },
  { value: "failures", label: "Сбои сессий", icon: <AlertTriangle /> },
  { value: "extra", label: "Ещё одна вкладка", icon: <Activity /> },
];

export default function OverflowCheckPage() {
  return (
    <div style={{ padding: 12, display: "flex", flexDirection: "column", gap: 24 }}>
      {/* Layer-1 guard: bare prose with a long URL must wrap, not h-scroll. */}
      <div data-overflow-check="prose" className="prose" style={{ width: 280, maxWidth: "100%" }}>
        <p>Смотри это видео: {LONG_URL} — оно про биты.</p>
      </div>

      {/* Layer-1 guard: bare mono value with a long URL must wrap, not h-scroll. */}
      <div
        data-overflow-check="mono"
        className="font-mono text-xs"
        style={{ width: 280, maxWidth: "100%" }}
      >
        {LONG_URL}
      </div>

      {/* Visual/screenshot only — tab bar overflow affordance. */}
      <div style={{ width: "100%", maxWidth: "100%" }}>
        <Tabs defaultValue="watchdog">
          <FilterTabsList items={TABS} />
        </Tabs>
      </div>

      {/* Visual/screenshot only — provider truncate row (min-w-0 fix). */}
      <div className="flex items-center gap-1.5 rounded-lg border p-2" style={{ width: 280 }}>
        <Globe className="h-4 w-4 shrink-0" />
        <span className="text-xs text-muted-foreground font-mono truncate min-w-0">{LONG_URL}</span>
      </div>
    </div>
  );
}
