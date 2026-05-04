// ui/src/__tests__/curator-ui.test.ts
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import React from "react";

// Mock @/lib/api before importing hooks
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(),
  apiPut: vi.fn(),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
}));

// Mock stores/hooks that queries.ts depends on but are irrelevant here
vi.mock("@/stores/notification-store", () => ({
  useNotificationStore: vi.fn(() => vi.fn()),
}));
vi.mock("@/hooks/use-ws-subscription", () => ({
  useWsSubscription: vi.fn(),
}));

import { apiGet } from "@/lib/api";
import { useCuratorConfig, useCuratorStatus, useCuratorRuns } from "@/lib/queries";
import type { CuratorConfig, CuratorStatus, CuratorRun } from "@/types/api";

function makeWrapper() {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return function Wrapper({ children }: { children: React.ReactNode }) {
    return React.createElement(QueryClientProvider, { client: qc }, children);
  };
}

const MOCK_CONFIG: CuratorConfig = {
  enabled: true,
  cron: "0 3 * * *",
  min_idle_minutes: 30,
  stale_after_days: 7,
  archive_after_days: 30,
  max_repairs_per_run: 5,
  agent_name: "Curator",
};

const MOCK_STATUS: CuratorStatus = {
  enabled: true,
  cron: "0 3 * * *",
  last_run_at: "2026-05-01T03:00:00Z",
  last_run_id: "run-uuid-1",
  last_phase1: 2,
  last_phase2: 0,
  last_phase3: 1,
};

const MOCK_RUNS: CuratorRun[] = [
  {
    id: "run-uuid-1",
    started_at: "2026-05-01T03:00:00Z",
    finished_at: "2026-05-01T03:00:05Z",
    duration_ms: 5000,
    triggered_by: "cron",
    phase1_transitions: 2,
    phase2_repairs: 0,
    phase3_commands: 1,
    skipped_reason: null,
    report_md: null,
    error: null,
  },
  {
    id: "run-uuid-2",
    started_at: "2026-04-30T03:00:00Z",
    finished_at: "2026-04-30T03:00:03Z",
    duration_ms: 3000,
    triggered_by: "manual",
    phase1_transitions: 0,
    phase2_repairs: 1,
    phase3_commands: 0,
    skipped_reason: null,
    report_md: null,
    error: null,
  },
];

beforeEach(() => {
  vi.clearAllMocks();
});

describe("useCuratorConfig", () => {
  it("curator_config_query_returns_data", async () => {
    vi.mocked(apiGet).mockResolvedValue(MOCK_CONFIG);

    const { result } = renderHook(() => useCuratorConfig(), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(apiGet).toHaveBeenCalledWith("/api/curator/config");
    const data = result.current.data as CuratorConfig;
    expect(data.enabled).toBe(true);
    expect(data.cron).toBe("0 3 * * *");
    expect(data.stale_after_days).toBe(7);
    expect(data.archive_after_days).toBe(30);
    expect(data.max_repairs_per_run).toBe(5);
    expect(data.agent_name).toBe("Curator");
  });

  it("returns error state when API call fails", async () => {
    vi.mocked(apiGet).mockRejectedValue(new Error("network error"));

    const { result } = renderHook(() => useCuratorConfig(), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error).toBeInstanceOf(Error);
  });
});

describe("useCuratorStatus", () => {
  it("curator_status_query_returns_data", async () => {
    vi.mocked(apiGet).mockResolvedValue(MOCK_STATUS);

    const { result } = renderHook(() => useCuratorStatus(), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(apiGet).toHaveBeenCalledWith("/api/curator/status");
    const data = result.current.data as CuratorStatus;
    expect(data.enabled).toBe(true);
    expect(data.cron).toBe("0 3 * * *");
    expect(data.last_run_at).toBe("2026-05-01T03:00:00Z");
    expect(data.last_run_id).toBe("run-uuid-1");
  });

  it("handles null last_run fields when curator has never run", async () => {
    const neverRun: CuratorStatus = {
      ...MOCK_STATUS,
      last_run_at: null,
      last_run_id: null,
      last_phase1: 0,
      last_phase2: 0,
      last_phase3: 0,
    };
    vi.mocked(apiGet).mockResolvedValue(neverRun);

    const { result } = renderHook(() => useCuratorStatus(), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const data = result.current.data as CuratorStatus;
    expect(data.last_run_at).toBeNull();
    expect(data.last_run_id).toBeNull();
  });
});

describe("useCuratorRuns", () => {
  it("curator_runs_query_returns_list", async () => {
    vi.mocked(apiGet).mockResolvedValue({ runs: MOCK_RUNS });

    const { result } = renderHook(() => useCuratorRuns(), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(apiGet).toHaveBeenCalledWith("/api/curator/runs");
    const runs = (result.current.data as { runs: CuratorRun[] }).runs;
    expect(runs).toHaveLength(2);
    expect(runs[0].id).toBe("run-uuid-1");
    expect(runs[0].triggered_by).toBe("cron");
    expect(runs[1].id).toBe("run-uuid-2");
    expect(runs[1].triggered_by).toBe("manual");
  });

  it("returns empty list when no runs exist", async () => {
    vi.mocked(apiGet).mockResolvedValue({ runs: [] });

    const { result } = renderHook(() => useCuratorRuns(), {
      wrapper: makeWrapper(),
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const runs = (result.current.data as { runs: CuratorRun[] }).runs;
    expect(runs).toHaveLength(0);
  });
});
