import { describe, it, expect } from "vitest";
import { parseSseEvent, parseSSELines } from "@/stores/sse-events";

// NOTE (S6.5): the parser is now a thin pass-through. Wire-shape correctness
// is guaranteed by codegen (Rust SseEvent → ts-rs → @/types/sse.generated.ts);
// per-variant runtime validation was removed. Tests below cover what the
// parser still does: JSON validity + presence of `type` field. Round-trip
// fixture tests in T7 cover the actual contract.

describe("parseSseEvent — JSON / type-field coverage", () => {
  it("parses data-session-id event", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "data-session-id",
        data: { sessionId: "sess-abc", contextLimit: null },
        transient: false,
      }),
    );
    expect(e).toEqual({
      type: "data-session-id",
      data: { sessionId: "sess-abc", contextLimit: null },
      transient: false,
    });
  });

  it("parses start event", () => {
    expect(
      parseSseEvent(JSON.stringify({ type: "start", messageId: "m1", agentName: "Alice" })),
    ).toEqual({ type: "start", messageId: "m1", agentName: "Alice" });
  });

  it("parses text-start", () => {
    expect(
      parseSseEvent(JSON.stringify({ type: "text-start", id: "t1", agentName: "Alice" })),
    ).toEqual({ type: "text-start", id: "t1", agentName: "Alice" });
  });

  it("parses text-end event", () => {
    expect(parseSseEvent(JSON.stringify({ type: "text-end", id: "t1" }))).toEqual({
      type: "text-end",
      id: "t1",
    });
  });

  it("parses tool-input-delta event", () => {
    const e = parseSseEvent(
      JSON.stringify({ type: "tool-input-delta", toolCallId: "tc1", inputTextDelta: '{"q":' }),
    );
    expect(e).toEqual({ type: "tool-input-delta", toolCallId: "tc1", inputTextDelta: '{"q":' });
  });

  it("parses tool-input-available event", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "tool-input-available",
        toolCallId: "tc1",
        toolName: "search",
        input: { query: "test" },
      }),
    );
    expect(e).toEqual({
      type: "tool-input-available",
      toolCallId: "tc1",
      toolName: "search",
      input: { query: "test" },
    });
  });

  it("parses file event", () => {
    const e = parseSseEvent(
      JSON.stringify({ type: "file", url: "/img.png", mediaType: "image/png" }),
    );
    expect(e).toEqual({ type: "file", url: "/img.png", mediaType: "image/png" });
  });

  it("parses rich-card event", () => {
    const e = parseSseEvent(
      JSON.stringify({ type: "rich-card", cardType: "table", data: { title: null, columns: [], rows: [] } }),
    );
    expect(e).toEqual({
      type: "rich-card",
      cardType: "table",
      data: { title: null, columns: [], rows: [] },
    });
  });

  it("parses sync event", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "sync",
        content: "hello",
        toolCalls: [{ id: "tc1" }],
        status: "finished",
        error: null,
      }),
    );
    expect(e).toEqual({
      type: "sync",
      content: "hello",
      toolCalls: [{ id: "tc1" }],
      status: "finished",
      error: null,
    });
  });

  it("parses error event", () => {
    expect(parseSseEvent(JSON.stringify({ type: "error", errorText: "boom" }))).toEqual({
      type: "error",
      errorText: "boom",
    });
  });

  it("parses step-start event", () => {
    const e = parseSseEvent(
      JSON.stringify({ type: "step-start", stepId: "step-1", messageId: "m1", agentName: "A" }),
    );
    expect(e).toEqual({ type: "step-start", stepId: "step-1", messageId: "m1", agentName: "A" });
  });

  it("parses reconnecting event", () => {
    expect(
      parseSseEvent(JSON.stringify({ type: "reconnecting", attempt: 2, delay_ms: 2000 })),
    ).toEqual({ type: "reconnecting", attempt: 2, delay_ms: 2000 });
  });

  it("parses tool-approval-needed event", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "tool-approval-needed",
        approvalId: "appr-1",
        toolName: "workspace_write",
        toolInput: { path: "/foo" },
        timeoutMs: 60000,
      }),
    );
    expect(e).toEqual({
      type: "tool-approval-needed",
      approvalId: "appr-1",
      toolName: "workspace_write",
      toolInput: { path: "/foo" },
      timeoutMs: 60000,
    });
  });

  it("parses tool-approval-resolved event for all action values", () => {
    for (const action of ["approved", "rejected", "timeout_rejected"] as const) {
      const e = parseSseEvent(
        JSON.stringify({ type: "tool-approval-resolved", approvalId: "appr-1", action, modifiedInput: null }),
      );
      expect(e).toEqual({
        type: "tool-approval-resolved",
        approvalId: "appr-1",
        action,
        modifiedInput: null,
      });
    }
  });

  it("parses tool-approval-resolved with modifiedInput", () => {
    const e = parseSseEvent(
      JSON.stringify({
        type: "tool-approval-resolved",
        approvalId: "appr-1",
        action: "approved",
        modifiedInput: { path: "/bar" },
      }),
    );
    expect(e).toEqual({
      type: "tool-approval-resolved",
      approvalId: "appr-1",
      action: "approved",
      modifiedInput: { path: "/bar" },
    });
  });

  it("returns null for non-object JSON", () => {
    expect(parseSseEvent('"just a string"')).toBeNull();
    expect(parseSseEvent("42")).toBeNull();
    expect(parseSseEvent("null")).toBeNull();
  });

  it("returns null for unparseable JSON", () => {
    expect(parseSseEvent("not json")).toBeNull();
  });

  it("returns null for missing type field", () => {
    expect(parseSseEvent(JSON.stringify({ delta: "oops" }))).toBeNull();
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
