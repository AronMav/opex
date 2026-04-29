import { vi, describe, it, expect, beforeEach } from "vitest";

// Mock auth store
const mockLogout = vi.fn();
vi.mock("@/stores/auth-store", () => ({
  useAuthStore: {
    getState: () => ({ token: mockToken, logout: mockLogout }),
  },
}));

// Mock fetch
const mockFetch = vi.fn();
vi.stubGlobal("fetch", mockFetch);

// Stub AbortSignal.any for jsdom
if (!AbortSignal.any) {
  (AbortSignal as any).any = (signals: AbortSignal[]) => signals[0];
}

// Dynamic token for per-test control
let mockToken = "test-token";

import { apiGet, apiPost, apiPut, apiPatch, apiDelete, getToken, assertToken, _resetRedirecting } from "@/lib/api";

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function textResponse(text: string, status: number) {
  return new Response(text, { status });
}

beforeEach(() => {
  mockFetch.mockReset();
  mockLogout.mockReset();
  mockToken = "test-token";
  _resetRedirecting();
});

describe("getToken", () => {
  it("returns token from auth store", () => {
    expect(getToken()).toBe("test-token");
  });
});

describe("apiGet", () => {
  it("sends GET with auth header when token exists", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ ok: true }));
    await apiGet("/api/test");

    expect(mockFetch).toHaveBeenCalledOnce();
    const [url, init] = mockFetch.mock.calls[0];
    expect(url).toBe("/api/test");
    expect(init.headers["Authorization"]).toBe("Bearer test-token");
    expect(init.headers["Content-Type"]).toBe("application/json");
  });

  it("throws Session expired when no token", async () => {
    mockToken = "";
    await expect(apiGet("/api/test")).rejects.toThrow("Session expired");
    expect(mockFetch).not.toHaveBeenCalled();
    expect(mockLogout).toHaveBeenCalledOnce();
  });

  it("throws on non-ok response with error from JSON body", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ error: "Not found" }, 404));
    await expect(apiGet("/api/missing")).rejects.toThrow("Not found");
  });

  it("throws on non-ok response with raw text when not JSON", async () => {
    mockFetch.mockResolvedValue(textResponse("bad gateway", 502));
    await expect(apiGet("/api/bad")).rejects.toThrow("bad gateway");
  });

  it("throws HTTP status when body is empty", async () => {
    mockFetch.mockResolvedValue(new Response("", { status: 500 }));
    await expect(apiGet("/api/fail")).rejects.toThrow("HTTP 500");
  });
});

describe("apiPost", () => {
  it("sends POST with JSON body", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ id: 1 }));
    const result = await apiPost("/api/items", { name: "test" });

    const [, init] = mockFetch.mock.calls[0];
    expect(init.method).toBe("POST");
    expect(init.body).toBe(JSON.stringify({ name: "test" }));
    expect(result).toEqual({ id: 1 });
  });

  it("sends POST without body when body is undefined", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ ok: true }));
    await apiPost("/api/action");

    const [, init] = mockFetch.mock.calls[0];
    expect(init.method).toBe("POST");
    expect(init.body).toBeUndefined();
  });

  it("merges extraHeaders into request headers", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ ok: true }));
    await apiPost("/api/action", { x: 1 }, { "X-Custom": "value" });

    const [, init] = mockFetch.mock.calls[0];
    expect(init.headers["X-Custom"]).toBe("value");
    expect(init.headers["Authorization"]).toBe("Bearer test-token");
  });
});

describe("apiPut", () => {
  it("sends PUT with JSON body", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ updated: true }));
    const result = await apiPut("/api/items/1", { name: "updated" });

    const [, init] = mockFetch.mock.calls[0];
    expect(init.method).toBe("PUT");
    expect(init.body).toBe(JSON.stringify({ name: "updated" }));
    expect(result).toEqual({ updated: true });
  });
});

describe("apiPatch", () => {
  it("sends PATCH with JSON body", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ patched: true }));
    const result = await apiPatch("/api/items/1", { status: "done" });

    const [, init] = mockFetch.mock.calls[0];
    expect(init.method).toBe("PATCH");
    expect(init.body).toBe(JSON.stringify({ status: "done" }));
    expect(result).toEqual({ patched: true });
  });
});

describe("apiDelete", () => {
  it("sends DELETE and returns void on success", async () => {
    mockFetch.mockResolvedValue(jsonResponse({}, 200));
    const result = await apiDelete("/api/items/1");

    const [, init] = mockFetch.mock.calls[0];
    expect(init.method).toBe("DELETE");
    expect(result).toBeUndefined();
  });

  it("throws on error response", async () => {
    mockFetch.mockResolvedValue(jsonResponse({ error: "Forbidden" }, 403));
    await expect(apiDelete("/api/items/1")).rejects.toThrow("Forbidden");
  });
});

describe("assertToken", () => {
  it("returns token when store has one", () => {
    expect(assertToken()).toBe("test-token");
  });

  it("throws Session expired when token is empty", () => {
    mockToken = "";
    expect(() => assertToken()).toThrow("Session expired");
    expect(mockLogout).toHaveBeenCalledOnce();
  });

  it("throws Session expired when already redirecting", () => {
    // Trigger redirecting state by calling assertToken with empty token first
    mockToken = "";
    expect(() => assertToken()).toThrow("Session expired");
    // Now even with a valid token, it should throw because redirecting is true
    mockToken = "valid-token";
    expect(() => assertToken()).toThrow("Session expired");
  });
});

describe("error handling", () => {
  it("401 response triggers logout and throws Session expired", async () => {
    mockFetch.mockResolvedValue(new Response("", { status: 401 }));
    await expect(apiGet("/api/test")).rejects.toThrow("Session expired");
    expect(mockLogout).toHaveBeenCalledOnce();
  });

  it("429 response throws rate limit error", async () => {
    mockFetch.mockResolvedValue(new Response("", { status: 429 }));
    await expect(apiGet("/api/test")).rejects.toThrow("Too many failed attempts");
  });
});
