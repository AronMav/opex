import { describe, it, expect, vi, beforeEach } from "vitest";
import { isBinaryFile } from "@/lib/api";

describe("isBinaryFile", () => {
  it("narrows binary responses", () => {
    expect(isBinaryFile({ is_binary: true, mime: "image/png", size: 1, url: "/x", path: "x.png", is_dir: false })).toBe(true);
    expect(isBinaryFile({ content: "hi", path: "n.md", is_dir: false })).toBe(false);
  });
});

describe("wsDeleteRecursive URL encoding", () => {
  beforeEach(() => {
    // mock fetch so apiDelete doesn't actually fire a request
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => ({}), text: async () => "" }));
    // stub auth store so assertToken doesn't throw
    vi.mock("@/stores/auth-store", () => ({
      useAuthStore: { getState: () => ({ token: "test-token", logout: vi.fn() }) },
    }));
  });

  it("encodes each path segment individually, preserving slashes", async () => {
    const { wsDeleteRecursive } = await import("@/lib/api");
    // spy on the constructed URL
    const fetched: string[] = [];
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      fetched.push(url);
      return Promise.resolve({ ok: true, status: 200, text: async () => "" } as Response);
    }));

    await wsDeleteRecursive("vault/My Note/a.md").catch(() => {});
    expect(fetched[0]).toBe("/api/workspace/vault/My%20Note/a.md?recursive=true");
  });

  it("encodes # and ? in segment names without breaking the query string", async () => {
    const { wsDeleteRecursive } = await import("@/lib/api");
    const fetched: string[] = [];
    vi.stubGlobal("fetch", vi.fn((url: string) => {
      fetched.push(url);
      return Promise.resolve({ ok: true, status: 200, text: async () => "" } as Response);
    }));

    await wsDeleteRecursive("folder/note#1.md").catch(() => {});
    expect(fetched[0]).toBe("/api/workspace/folder/note%231.md?recursive=true");
  });
});
