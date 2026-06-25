import { describe, it, expect, vi, beforeEach } from "vitest";

// Mock auth-store so module-level code in api.ts initialises cleanly
vi.mock("@/stores/auth-store", () => ({
  useAuthStore: {
    getState: vi.fn().mockReturnValue({ token: "test-token", logout: vi.fn() }),
    subscribe: vi.fn(),
  },
}));

// Intercept fetch at the global level — api.ts calls fetch() directly inside apiFetch()
const mockFetch = vi.fn().mockResolvedValue({
  ok: true,
  status: 200,
  json: () => Promise.resolve({}),
});
vi.stubGlobal("fetch", mockFetch);

import { listCheckpoints, diffCheckpoint, restoreCheckpoint } from "@/lib/api";

describe("checkpoint api fns", () => {
  beforeEach(() => vi.clearAllMocks());

  it("listCheckpoints бьёт в GET /api/agents/{name}/checkpoints", async () => {
    mockFetch.mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve({ enabled: true, items: [] }) });
    await listCheckpoints("Agent");
    expect(mockFetch).toHaveBeenCalledWith(
      "/api/agents/Agent/checkpoints",
      expect.objectContaining({ headers: expect.objectContaining({ Authorization: "Bearer test-token" }) }),
    );
  });

  it("listCheckpoints кодирует имя с пробелом", async () => {
    mockFetch.mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve({ enabled: false, items: [] }) });
    await listCheckpoints("My Agent");
    expect(mockFetch).toHaveBeenCalledWith(
      "/api/agents/My%20Agent/checkpoints",
      expect.anything(),
    );
  });

  it("diffCheckpoint бьёт в GET /api/agents/{name}/checkpoints/{n}/diff", async () => {
    mockFetch.mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve({ diff: "" }) });
    await diffCheckpoint("Agent", 3);
    expect(mockFetch).toHaveBeenCalledWith(
      "/api/agents/Agent/checkpoints/3/diff",
      expect.anything(),
    );
  });

  it("restoreCheckpoint бьёт в POST /api/agents/{name}/checkpoints/{n}/restore без file", async () => {
    mockFetch.mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve({ n: 3, files: [], new_checkpoint: 4 }) });
    await restoreCheckpoint("Agent", 3);
    const [url, init] = mockFetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/agents/Agent/checkpoints/3/restore");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({});
  });

  it("restoreCheckpoint передаёт file если задан", async () => {
    mockFetch.mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve({ n: 2, files: ["SOUL.md"], new_checkpoint: 5 }) });
    await restoreCheckpoint("Agent", 2, "SOUL.md");
    const [url, init] = mockFetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/agents/Agent/checkpoints/2/restore");
    expect(JSON.parse(init.body as string)).toEqual({ file: "SOUL.md" });
  });
});
