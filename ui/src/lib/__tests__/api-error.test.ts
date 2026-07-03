import { describe, it, expect, vi, beforeEach } from "vitest";

// D3 (design-review phase 2): error messages surfaced to banners/toasts must be
// human-readable — raw HTML bodies (dev-server 404 pages, proxy/gateway error
// pages) must collapse to a concise "HTTP <status>".

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: { getState: () => ({ token: "test-token", logout: vi.fn() }) },
}));

function stubFetch(status: number, body: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue({
      ok: status >= 200 && status < 300,
      status,
      json: async () => JSON.parse(body),
      text: async () => body,
    } as unknown as Response),
  );
}

describe("apiGet error extraction", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("collapses an HTML error page (<!DOCTYPE ...>) to HTTP <status>", async () => {
    const { apiGet } = await import("@/lib/api");
    stubFetch(404, '<!DOCTYPE html><html lang="ru"><head><meta charSet="utf-8"/></head><body>404</body></html>');
    await expect(apiGet("/api/providers")).rejects.toThrow(/^HTTP 404$/);
  });

  it("collapses an HTML body without doctype (<html ...>) to HTTP <status>", async () => {
    const { apiGet } = await import("@/lib/api");
    stubFetch(502, '\n  <html><body>Bad Gateway</body></html>');
    await expect(apiGet("/api/agents")).rejects.toThrow(/^HTTP 502$/);
  });

  it("keeps the backend's JSON {error} message intact", async () => {
    const { apiGet } = await import("@/lib/api");
    stubFetch(400, JSON.stringify({ error: "agent not found" }));
    await expect(apiGet("/api/agents/x")).rejects.toThrow("agent not found");
  });

  it("keeps short plain-text bodies intact", async () => {
    const { apiGet } = await import("@/lib/api");
    stubFetch(500, "database connection refused");
    await expect(apiGet("/api/memory")).rejects.toThrow("database connection refused");
  });

  it("clamps very long plain-text bodies to 300 chars", async () => {
    const { apiGet } = await import("@/lib/api");
    stubFetch(500, "x".repeat(1000));
    const err = await apiGet("/api/logs").catch((e: Error) => e);
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message.length).toBeLessThanOrEqual(301);
    expect((err as Error).message.endsWith("…")).toBe(true);
  });

  it("falls back to HTTP <status> for an empty body", async () => {
    const { apiGet } = await import("@/lib/api");
    stubFetch(503, "");
    await expect(apiGet("/api/health")).rejects.toThrow(/^HTTP 503$/);
  });
});
