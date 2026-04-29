import { vi, describe, it, expect } from "vitest";

// Mock dependencies before importing chat-store
vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: vi.fn(), getQueryData: vi.fn(() => undefined) },
}));
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
  getToken: vi.fn(() => "test-token"),
  assertToken: vi.fn(() => "test-token"),
}));

import { contentHash, reconcileLiveWithHistory } from "@/stores/chat-store";
import type { ChatMessage } from "@/stores/chat-types";

// ── Helper ─────────────────────────────────────────────────────────────────

function makeMsg(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "msg-1",
    role: "user",
    parts: [{ type: "text", text: "Hello" }],
    createdAt: new Date().toISOString(),
    ...overrides,
  };
}

// ── contentHash ────────────────────────────────────────────────────────────

describe("contentHash", () => {
  it("HASH-01: returns a stable string for identical message arrays", () => {
    const msgs: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Hello" }] }),
      makeMsg({ id: "m2", role: "assistant", parts: [{ type: "text", text: "Hi" }] }),
    ];
    const h1 = contentHash(msgs);
    const h2 = contentHash(msgs);
    expect(h1).toBe(h2);
    expect(typeof h1).toBe("string");
    expect(h1.length).toBeGreaterThan(0);
  });

  it("HASH-02: returns different hash for different content", () => {
    const msgs1: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Hello" }] }),
    ];
    const msgs2: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Goodbye" }] }),
    ];
    expect(contentHash(msgs1)).not.toBe(contentHash(msgs2));
  });

  it("HASH-03: ignores createdAt differences", () => {
    const msgs1: ChatMessage[] = [
      makeMsg({ id: "m1", createdAt: "2026-01-01T00:00:00Z", parts: [{ type: "text", text: "Hello" }] }),
    ];
    const msgs2: ChatMessage[] = [
      makeMsg({ id: "m1", createdAt: "2026-06-15T12:30:00Z", parts: [{ type: "text", text: "Hello" }] }),
    ];
    expect(contentHash(msgs1)).toBe(contentHash(msgs2));
  });

  it("HASH-04: returns a known constant for empty array", () => {
    const h1 = contentHash([]);
    const h2 = contentHash([]);
    expect(h1).toBe(h2);
    expect(typeof h1).toBe("string");
  });
});

// ── reconcileLiveWithHistory ───────────────────────────────────────────────

describe("reconcileLiveWithHistory", () => {
  it("RECONCILE-01: returns null when live and history have same IDs and content", () => {
    const msgs: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Hello" }] }),
      makeMsg({ id: "m2", role: "assistant", parts: [{ type: "text", text: "Hi" }] }),
    ];
    // Create separate arrays with same content (different createdAt to prove it's ignored)
    const live = msgs.map(m => ({ ...m, createdAt: "2026-01-01T00:00:00Z" }));
    const history = msgs.map(m => ({ ...m, createdAt: "2026-06-15T12:00:00Z" }));
    expect(reconcileLiveWithHistory(live, history)).toBeNull();
  });

  it("RECONCILE-02: returns history when history has extra messages", () => {
    const live: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Hello" }] }),
    ];
    const history: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Hello" }] }),
      makeMsg({ id: "m2", role: "assistant", parts: [{ type: "text", text: "Response with tool results" }] }),
    ];
    const result = reconcileLiveWithHistory(live, history);
    expect(result).not.toBeNull();
    expect(result).toHaveLength(2);
  });

  it("RECONCILE-03: returns history when IDs match but content differs", () => {
    const live: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Hello" }] }),
      makeMsg({ id: "m2", role: "assistant", parts: [{ type: "text", text: "Raw response" }] }),
    ];
    const history: ChatMessage[] = [
      makeMsg({ id: "m1", parts: [{ type: "text", text: "Hello" }] }),
      makeMsg({ id: "m2", role: "assistant", parts: [{ type: "text", text: "Post-processed response" }] }),
    ];
    const result = reconcileLiveWithHistory(live, history);
    expect(result).not.toBeNull();
    expect(result).toEqual(history);
  });

  it("STABILITY-01: user message with client UUID differs from DB ID in history", () => {
    const live: ChatMessage[] = [
      makeMsg({ id: "client-uuid-abc", role: "user", parts: [{ type: "text", text: "Hello" }] }),
      makeMsg({ id: "db-assist-1", role: "assistant", parts: [{ type: "text", text: "Hi" }] }),
    ];
    const history: ChatMessage[] = [
      makeMsg({ id: "db-user-1", role: "user", parts: [{ type: "text", text: "Hello" }] }),
      makeMsg({ id: "db-assist-1", role: "assistant", parts: [{ type: "text", text: "Hi" }] }),
    ];
    // IDs differ for user message, so reconciliation should return history
    const result = reconcileLiveWithHistory(live, history);
    expect(result).not.toBeNull();
    expect(result).toEqual(history);
  });
});
