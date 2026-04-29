import { describe, it, expect } from "vitest";
import { parseSseEvent, parseSSELines } from "@/stores/sse-events";

describe("parseSseEvent — full coverage", () => {
  it("parses data-session-id event", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "data-session-id",
      data: { sessionId: "sess-abc" },
    }));
    expect(e).toEqual({ type: "data-session-id", data: { sessionId: "sess-abc" } });
  });

  it("returns null for data-session-id with missing sessionId", () => {
    expect(parseSseEvent(JSON.stringify({ type: "data-session-id", data: {} }))).toBeNull();
    expect(parseSseEvent(JSON.stringify({ type: "data-session-id" }))).toBeNull();
  });

  it("parses start event with optional messageId and agentName", () => {
    expect(parseSseEvent(JSON.stringify({ type: "start", messageId: "m1", agentName: "Alice" }))).toEqual({
      type: "start",
      messageId: "m1",
      agentName: "Alice",
    });
    expect(parseSseEvent(JSON.stringify({ type: "start" }))).toEqual({
      type: "start",
      messageId: undefined,
      agentName: undefined,
    });
  });

  it("parses text-start with optional id and agentName", () => {
    expect(parseSseEvent(JSON.stringify({ type: "text-start", id: "t1", agentName: "Alice" }))).toEqual({
      type: "text-start",
      id: "t1",
      agentName: "Alice",
    });
    expect(parseSseEvent(JSON.stringify({ type: "text-start" }))).toEqual({
      type: "text-start",
      id: undefined,
      agentName: undefined,
    });
  });

  it("parses text-end event", () => {
    expect(parseSseEvent(JSON.stringify({ type: "text-end" }))).toEqual({ type: "text-end" });
  });

  it("parses tool-input-delta event", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "tool-input-delta",
      toolCallId: "tc1",
      inputTextDelta: '{"q":',
    }));
    expect(e).toEqual({
      type: "tool-input-delta",
      toolCallId: "tc1",
      inputTextDelta: '{"q":',
    });
  });

  it("returns null for tool-input-delta without toolCallId", () => {
    expect(parseSseEvent(JSON.stringify({ type: "tool-input-delta" }))).toBeNull();
  });

  it("parses tool-input-available event", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "tool-input-available",
      toolCallId: "tc1",
      input: { query: "test" },
    }));
    expect(e).toEqual({
      type: "tool-input-available",
      toolCallId: "tc1",
      input: { query: "test" },
    });
  });

  it("defaults tool-input-available input to empty object", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "tool-input-available",
      toolCallId: "tc1",
    }));
    expect(e?.type === "tool-input-available" && e.input).toEqual({});
  });

  it("parses file event", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "file",
      url: "/img.png",
      mediaType: "image/png",
    }));
    expect(e).toEqual({ type: "file", url: "/img.png", mediaType: "image/png" });
  });

  it("returns null for file without url", () => {
    expect(parseSseEvent(JSON.stringify({ type: "file" }))).toBeNull();
  });

  it("parses file event without mediaType", () => {
    const e = parseSseEvent(JSON.stringify({ type: "file", url: "/f.bin" }));
    expect(e).toEqual({ type: "file", url: "/f.bin", mediaType: undefined });
  });

  it("parses rich-card event", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "rich-card",
      cardType: "table",
      data: { rows: [] },
    }));
    expect(e).toEqual({ type: "rich-card", cardType: "table", data: { rows: [] } });
  });

  it("parses sync event", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "sync",
      content: "hello",
      toolCalls: [{ id: "tc1" }],
      status: "complete",
      error: "oops",
    }));
    expect(e).toEqual({
      type: "sync",
      content: "hello",
      toolCalls: [{ id: "tc1" }],
      status: "complete",
      error: "oops",
    });
  });

  it("defaults sync fields when missing", () => {
    const e = parseSseEvent(JSON.stringify({ type: "sync" }));
    expect(e).toEqual({
      type: "sync",
      content: "",
      toolCalls: [],
      status: "unknown",
      error: undefined,
    });
  });

  it("parses error event with default errorText", () => {
    const e = parseSseEvent(JSON.stringify({ type: "error" }));
    expect(e).toEqual({ type: "error", errorText: "Unknown error" });
  });

  it("parses step-start event", () => {
    const e = parseSseEvent(JSON.stringify({ type: "step-start", stepId: "step-1" }));
    expect(e).toEqual({ type: "step-start", stepId: "step-1" });
  });

  it("returns null for step-start without stepId", () => {
    expect(parseSseEvent(JSON.stringify({ type: "step-start" }))).toBeNull();
  });

  it("parses step-finish event with optional finishReason default", () => {
    expect(parseSseEvent(JSON.stringify({ type: "step-finish", stepId: "step-1", finishReason: "stop" }))).toEqual({
      type: "step-finish",
      stepId: "step-1",
      finishReason: "stop",
    });
    expect(parseSseEvent(JSON.stringify({ type: "step-finish", stepId: "step-1" }))).toEqual({
      type: "step-finish",
      stepId: "step-1",
      finishReason: "unknown",
    });
  });

  it("parses reconnecting event with defaults", () => {
    expect(parseSseEvent(JSON.stringify({ type: "reconnecting", attempt: 2, delay_ms: 2000 }))).toEqual({
      type: "reconnecting",
      attempt: 2,
      delay_ms: 2000,
    });
    expect(parseSseEvent(JSON.stringify({ type: "reconnecting" }))).toEqual({
      type: "reconnecting",
      attempt: 1,
      delay_ms: 2000,
    });
  });

  it("parses tool-approval-needed event", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "tool-approval-needed",
      approvalId: "appr-1",
      toolName: "workspace_write",
      toolInput: { path: "/foo" },
      timeoutMs: 60000,
    }));
    expect(e).toEqual({
      type: "tool-approval-needed",
      approvalId: "appr-1",
      toolName: "workspace_write",
      toolInput: { path: "/foo" },
      timeoutMs: 60000,
    });
  });

  it("defaults tool-approval-needed timeoutMs to 300000 and toolInput to empty object", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "tool-approval-needed",
      approvalId: "appr-1",
      toolName: "workspace_write",
    }));
    expect(e).toEqual({
      type: "tool-approval-needed",
      approvalId: "appr-1",
      toolName: "workspace_write",
      toolInput: {},
      timeoutMs: 300000,
    });
  });

  it("returns null for tool-approval-needed without required fields", () => {
    expect(parseSseEvent(JSON.stringify({ type: "tool-approval-needed", approvalId: "a" }))).toBeNull();
    expect(parseSseEvent(JSON.stringify({ type: "tool-approval-needed", toolName: "x" }))).toBeNull();
  });

  it("parses tool-approval-resolved event for all action values", () => {
    for (const action of ["approved", "rejected", "timeout_rejected"] as const) {
      const e = parseSseEvent(JSON.stringify({ type: "tool-approval-resolved", approvalId: "appr-1", action }));
      expect(e).toEqual({ type: "tool-approval-resolved", approvalId: "appr-1", action, modifiedInput: undefined });
    }
  });

  it("parses tool-approval-resolved with modifiedInput", () => {
    const e = parseSseEvent(JSON.stringify({
      type: "tool-approval-resolved",
      approvalId: "appr-1",
      action: "approved",
      modifiedInput: { path: "/bar" },
    }));
    expect(e).toEqual({
      type: "tool-approval-resolved",
      approvalId: "appr-1",
      action: "approved",
      modifiedInput: { path: "/bar" },
    });
  });

  it("returns null for tool-approval-resolved with invalid action", () => {
    expect(parseSseEvent(JSON.stringify({ type: "tool-approval-resolved", approvalId: "a", action: "unknown" }))).toBeNull();
  });

  it("returns null for tool-approval-resolved without approvalId", () => {
    expect(parseSseEvent(JSON.stringify({ type: "tool-approval-resolved", action: "approved" }))).toBeNull();
  });

  it("returns null for non-object JSON", () => {
    expect(parseSseEvent('"just a string"')).toBeNull();
    expect(parseSseEvent("42")).toBeNull();
    expect(parseSseEvent("null")).toBeNull();
  });

  it("returns null for non-string type field", () => {
    expect(parseSseEvent(JSON.stringify({ type: 123 }))).toBeNull();
  });
});

describe("parseSSELines — edge cases", () => {
  it("handles empty chunk", () => {
    const buf = { current: "" };
    expect(parseSSELines("", buf)).toEqual([]);
    expect(buf.current).toBe("");
  });

  it("handles multiple newlines producing empty lines", () => {
    const buf = { current: "" };
    const lines = parseSSELines("\n\n", buf);
    expect(lines).toEqual(["", ""]);
  });

  it("accumulates across multiple calls", () => {
    const buf = { current: "" };
    parseSSELines("data: par", buf);
    parseSSELines("tial\ndata: ", buf);
    const lines = parseSSELines("complete\n", buf);
    expect(lines).toEqual(["data: complete"]);
  });
});
