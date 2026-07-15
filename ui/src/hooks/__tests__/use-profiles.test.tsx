import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import React from "react";

// Mock @/lib/api before importing the hooks under test (same pattern as
// ui/src/__tests__/curator-ui.test.ts).
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(),
  apiPut: vi.fn(),
  apiDelete: vi.fn(),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import { apiGet, apiPost, apiDelete } from "@/lib/api";
import {
  useProfiles,
  useCreateProfile,
  useDeleteProfile,
  PROFILE_CAPABILITIES,
  type ProfileRow,
} from "@/hooks/use-profiles";

function makeClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function wrapperFor(qc: QueryClient) {
  return function Wrapper({ children }: { children: React.ReactNode }) {
    return React.createElement(QueryClientProvider, { client: qc }, children);
  };
}

const MOCK_PROFILE: ProfileRow = {
  id: "p1",
  name: "Default",
  slots: { text: [{ provider: "openai", model: "gpt-5" }] },
  agents: ["Opex"],
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

beforeEach(() => {
  vi.clearAllMocks();
});

describe("PROFILE_CAPABILITIES", () => {
  it("is the fixed 7-capability set", () => {
    expect(PROFILE_CAPABILITIES).toEqual([
      "text",
      "compaction",
      "stt",
      "tts",
      "vision",
      "imagegen",
      "websearch",
    ]);
  });
});

describe("useProfiles", () => {
  it("fetches GET /api/profiles and parses the profiles list", async () => {
    vi.mocked(apiGet).mockResolvedValue({ profiles: [MOCK_PROFILE] });

    const { result } = renderHook(() => useProfiles(), { wrapper: wrapperFor(makeClient()) });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(apiGet).toHaveBeenCalledWith("/api/profiles");
    expect(result.current.data?.profiles).toHaveLength(1);
    expect(result.current.data?.profiles[0]).toEqual(MOCK_PROFILE);
  });

  it("surfaces an error state when the request fails", async () => {
    vi.mocked(apiGet).mockRejectedValue(new Error("network error"));

    const { result } = renderHook(() => useProfiles(), { wrapper: wrapperFor(makeClient()) });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error).toBeInstanceOf(Error);
  });
});

describe("useCreateProfile", () => {
  it("posts to /api/profiles and invalidates the [\"profiles\"] query", async () => {
    vi.mocked(apiGet).mockResolvedValue({ profiles: [] });
    vi.mocked(apiPost).mockResolvedValue(MOCK_PROFILE);

    const qc = makeClient();
    const wrapper = wrapperFor(qc);
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useCreateProfile(), { wrapper });
    await act(async () => {
      await result.current.mutateAsync({ name: "Default" });
    });

    expect(apiPost).toHaveBeenCalledWith("/api/profiles", { name: "Default" });
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: ["profiles"] });
  });
});

describe("useDeleteProfile", () => {
  it("sends DELETE to /api/profiles/{id} and invalidates the profiles query", async () => {
    vi.mocked(apiDelete).mockResolvedValue(undefined);

    const qc = makeClient();
    const wrapper = wrapperFor(qc);
    const invalidateSpy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(() => useDeleteProfile(), { wrapper });
    await act(async () => {
      await result.current.mutateAsync("p1");
    });

    expect(apiDelete).toHaveBeenCalledWith("/api/profiles/p1");
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: ["profiles"] });
  });
});

describe("useProfiles + useCreateProfile integration", () => {
  it("an active useProfiles() list refetches and reflects new data after a create mutation", async () => {
    vi.mocked(apiGet).mockResolvedValueOnce({ profiles: [] });
    vi.mocked(apiPost).mockResolvedValue(MOCK_PROFILE);

    const qc = makeClient();
    const wrapper = wrapperFor(qc);

    const { result: listResult } = renderHook(() => useProfiles(), { wrapper });
    await waitFor(() => expect(listResult.current.isSuccess).toBe(true));
    expect(listResult.current.data?.profiles).toHaveLength(0);

    // Subsequent apiGet calls (the invalidation-triggered refetch) return the
    // newly created profile.
    vi.mocked(apiGet).mockResolvedValue({ profiles: [MOCK_PROFILE] });

    const { result: mutResult } = renderHook(() => useCreateProfile(), { wrapper });
    await act(async () => {
      await mutResult.current.mutateAsync({ name: "Default" });
    });

    await waitFor(() => expect(listResult.current.data?.profiles).toHaveLength(1));
    expect(listResult.current.data?.profiles[0]).toEqual(MOCK_PROFILE);
  });
});
