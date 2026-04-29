import { describe, it, expect } from "vitest";

import type { ConnectionPhase, MessageSource } from "@/stores/chat-store";

// ── Pure logic extracted from ChatThread.tsx ────────────────────────────────

/**
 * OPTI-01: showThinking computation.
 * Mirrors ChatThread.tsx:
 *   showThinking = messageSource.mode === "live"
 *     && (connectionPhase === "submitted" || (engineRunning && !hasAssistantContent))
 */
function computeShowThinking(
  messageSource: MessageSource,
  connectionPhase: ConnectionPhase,
  engineRunning: boolean,
  hasAssistantContent: boolean,
): boolean {
  return (
    messageSource.mode === "live" &&
    (connectionPhase === "submitted" || (engineRunning && !hasAssistantContent))
  );
}

/**
 * OPTI-02: skeleton display guard.
 * Mirrors ChatThread.tsx:
 *   showSkeleton = historyLoading && !sessionMessagesData && messageSource.mode !== "live"
 */
function computeShowSkeleton(
  historyLoading: boolean,
  sessionMessagesData: unknown | undefined,
  messageSource: MessageSource,
): boolean {
  return historyLoading && !sessionMessagesData && messageSource.mode !== "live";
}

// ── OPTI-01: Instant thinking indicator ────────────────────────────────────

describe("OPTI-01: showThinking contract", () => {
  it("is true when connectionPhase=submitted and mode=live", () => {
    // After sendMessage(), connectionPhase is set to "submitted" synchronously.
    // Guarantees the thinking indicator appears instantly — before any SSE event.
    expect(computeShowThinking({ mode: "live", messages: [] }, "submitted", false, false)).toBe(true);
  });

  it("is true when engineRunning and no assistant content yet", () => {
    // After page reload, engine may still be running but SSE not connected.
    expect(computeShowThinking({ mode: "live", messages: [] }, "idle", true, false)).toBe(true);
  });

  it("is false when mode=new-chat (no ghost thinking on empty chat)", () => {
    expect(computeShowThinking({ mode: "new-chat" }, "submitted", false, false)).toBe(false);
  });

  it("is false when mode=history (not a live stream)", () => {
    expect(computeShowThinking({ mode: "history", sessionId: "abc" }, "submitted", false, false)).toBe(false);
  });

  it("is false when engineRunning but assistant content already exists", () => {
    // Thinking indicator hides once the assistant starts writing
    expect(computeShowThinking({ mode: "live", messages: [] }, "idle", true, true)).toBe(false);
  });

  it("is false when mode=live but neither submitted nor engineRunning", () => {
    expect(computeShowThinking({ mode: "live", messages: [] }, "idle", false, false)).toBe(false);
  });
});

// ── OPTI-02: Agent-switch skeleton guard ───────────────────────────────────

describe("OPTI-02: skeleton display guard contract", () => {
  it("renders when historyLoading=true, no cache, and mode !== live", () => {
    expect(computeShowSkeleton(true, undefined, { mode: "history", sessionId: "abc" })).toBe(true);
  });

  it("does NOT render when sessionMessagesData exists in cache", () => {
    const cachedData = { messages: [{ id: "1", role: "assistant", parts: [] }] };
    expect(computeShowSkeleton(true, cachedData, { mode: "history", sessionId: "abc" })).toBe(false);
  });

  it("does NOT render when messageSource.mode=live", () => {
    expect(computeShowSkeleton(true, undefined, { mode: "live", messages: [] })).toBe(false);
  });

  it("does NOT render when historyLoading=false", () => {
    expect(computeShowSkeleton(false, undefined, { mode: "history", sessionId: "abc" })).toBe(false);
  });

  it("renders when mode=new-chat (not live) and historyLoading=true (edge case)", () => {
    // "new-chat" is not "live", so guard passes — the caller normally prevents
    // historyLoading=true when no session is selected, but the function itself allows it.
    expect(computeShowSkeleton(true, undefined, { mode: "new-chat" })).toBe(true);
  });
});
