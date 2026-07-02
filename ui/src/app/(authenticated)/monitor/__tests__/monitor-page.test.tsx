import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, within } from "@testing-library/react";

// ── Deterministic tab: default (?tab absent) → "watchdog" ────────────────────
vi.mock("next/navigation", () => ({
  useSearchParams: () => new URLSearchParams(""),
  useRouter: () => ({ push: vi.fn() }),
}));

// ── Doctor + watchdog useQuery ───────────────────────────────────────────────
// Only the Doctor tab uses useQuery directly; return a benign healthy payload.
vi.mock("@tanstack/react-query", () => ({
  useQuery: () => ({
    data: { ok: true, checks: {} },
    isLoading: false,
    error: null,
    refetch: vi.fn(),
    isFetching: false,
  }),
}));

// ── Query hooks used by the tabs ─────────────────────────────────────────────
vi.mock("@/lib/queries", () => ({
  useUsage: () => ({ data: { days: 30, usage: [] }, error: null, isLoading: false }),
  useDailyUsage: () => ({ data: { daily: [] }, error: null, isLoading: false }),
  useApprovals: () => ({ data: [], isLoading: false, error: null, refetch: vi.fn() }),
  useResolveApproval: () => ({ mutateAsync: vi.fn() }),
  useAudit: () => ({ data: [], isFetching: false, refetch: vi.fn() }),
  useSessionFailures: () => ({ data: { failures: [], total: 0 }, isFetching: false, refetch: vi.fn() }),
  useCuratorRuns: () => ({ data: { runs: [] }, isLoading: false }),
}));

// ── Watchdog data (fetched via apiGet in an effect) ──────────────────────────
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn((path: string) => {
    if (path === "/api/status") {
      return Promise.resolve({
        status: "ok",
        version: "0.2.0",
        uptime_seconds: 3600,
        memory_chunks: 42,
        agents: ["Opex"],
        active_sessions: 1,
        tools_registered: 12,
        scheduled_jobs: 3,
      });
    }
    if (path === "/api/stats") {
      return Promise.resolve({
        messages_today: 5,
        total_messages: 100,
        sessions_today: 2,
        total_sessions: 20,
      });
    }
    // /api/watchdog/status → no watchdog checks (keeps status card on the fallback path)
    return Promise.resolve(null);
  }),
  apiPost: vi.fn(() => Promise.resolve({})),
}));

vi.mock("@/hooks/use-auto-refresh", () => ({ useAutoRefresh: () => {} }));
vi.mock("@/hooks/use-ws-subscription", () => ({ useWsSubscription: () => {} }));
vi.mock("@/stores/ws-store", () => ({
  // useWsStore(selector) → selector picks ws/connected off a static snapshot.
  useWsStore: (selector: (s: { ws: null; connected: boolean }) => unknown) =>
    selector({ ws: null, connected: false }),
}));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import MonitorPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("renders the 8-tab shell", () => {
  render(<MonitorPage />);
  // All 8 tab triggers are present (forceMount + tab labels via i18n keys).
  for (const key of [
    "monitor.tab_watchdog",
    "monitor.tab_doctor",
    "monitor.tab_logs",
    "monitor.tab_audit",
    "monitor.tab_statistics",
    "monitor.tab_approvals",
    "monitor.tab_failures",
    "monitor.tab_curator",
  ]) {
    expect(screen.getByRole("tab", { name: key })).toBeInTheDocument();
  }
});

test("watchdog tab renders StatCards populated from /api/status", async () => {
  render(<MonitorPage />);
  // StatCard for uptime label is rendered...
  expect(await screen.findByText("dashboard.uptime")).toBeInTheDocument();
  // ...and the status card shows the version subtext once /api/status resolves.
  expect(await screen.findByText("0.2.0")).toBeInTheDocument();
});

test("doctor tab shows the healthy status banner (semantic tokens)", () => {
  render(<MonitorPage />);
  // Doctor's forceMount content is present; ok:true → all_ok banner.
  const banner = screen.getByText("doctor.all_ok");
  expect(banner).toBeInTheDocument();
  // The banner uses the success token, not a raw palette color.
  expect(banner.className).toContain("text-success");
  expect(within(banner).queryByText(/emerald|green-\d/)).not.toBeInTheDocument();
});
