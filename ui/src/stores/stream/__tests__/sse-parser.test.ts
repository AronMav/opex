import { describe, it, expect } from "vitest";
import { parseSseEvent, parseSSELines } from "../sse-parser";

describe("parseSseEvent", () => {
  it("parses data-session-id events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "data-session-id", data: { sessionId: "abc" } }));
    expect(event).toEqual({ type: "data-session-id", data: { sessionId: "abc" } });
  });

  it("returns null for unparseable JSON", () => {
    expect(parseSseEvent("not json")).toBeNull();
  });

  it("parses text-delta events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "text-delta", delta: "hello", id: "t1" }));
    expect(event?.type).toBe("text-delta");
  });

  it("parses text-start events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "text-start", id: "t1" }));
    expect(event?.type).toBe("text-start");
  });

  it("parses text-end events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "text-end", id: "t1" }));
    expect(event?.type).toBe("text-end");
  });

  it("parses finish events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "finish" }));
    expect(event?.type).toBe("finish");
  });

  it("parses error events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "error", errorText: "boom" }));
    expect(event?.type).toBe("error");
  });

  it("returns null for data-session-id missing sessionId", () => {
    expect(parseSseEvent(JSON.stringify({ type: "data-session-id", data: {} }))).toBeNull();
  });

  it("returns null for unknown event type", () => {
    expect(parseSseEvent(JSON.stringify({ type: "unknown-future-event" }))).toBeNull();
  });

  it("returns null for missing type field", () => {
    expect(parseSseEvent(JSON.stringify({ delta: "oops" }))).toBeNull();
  });

  it("returns null for non-object JSON", () => {
    expect(parseSseEvent('"just a string"')).toBeNull();
    expect(parseSseEvent("42")).toBeNull();
    expect(parseSseEvent("null")).toBeNull();
  });

  it("parses tool-input-start events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "tool-input-start", toolCallId: "tc1", toolName: "search" }));
    expect(event?.type).toBe("tool-input-start");
    if (event?.type === "tool-input-start") {
      expect(event.toolCallId).toBe("tc1");
      expect(event.toolName).toBe("search");
    }
  });

  it("returns null for tool-input-start missing required fields", () => {
    expect(parseSseEvent(JSON.stringify({ type: "tool-input-start" }))).toBeNull();
  });

  it("parses tool-output-available events", () => {
    const event = parseSseEvent(JSON.stringify({ type: "tool-output-available", toolCallId: "tc1", output: "result" }));
    expect(event?.type).toBe("tool-output-available");
  });

  it("parses sync events with defaults for missing fields", () => {
    const event = parseSseEvent(JSON.stringify({ type: "sync" }));
    expect(event?.type).toBe("sync");
    if (event?.type === "sync") {
      expect(event.content).toBe("");
      expect(event.toolCalls).toEqual([]);
      expect(event.status).toBe("unknown");
    }
  });

  it("parses tool-approval-needed events", () => {
    const event = parseSseEvent(JSON.stringify({
      type: "tool-approval-needed",
      approvalId: "a1",
      toolName: "workspace_write",
      toolInput: { path: "/foo" },
      timeoutMs: 60000,
    }));
    expect(event?.type).toBe("tool-approval-needed");
    if (event?.type === "tool-approval-needed") {
      expect(event.approvalId).toBe("a1");
      expect(event.timeoutMs).toBe(60000);
    }
  });

  it("parses tool-approval-resolved events", () => {
    const event = parseSseEvent(JSON.stringify({
      type: "tool-approval-resolved",
      approvalId: "a1",
      action: "approved",
    }));
    expect(event?.type).toBe("tool-approval-resolved");
    if (event?.type === "tool-approval-resolved") {
      expect(event.action).toBe("approved");
    }
  });

  it("parses reconnecting events with attempt and delay_ms", () => {
    const event = parseSseEvent(JSON.stringify({ type: "reconnecting", attempt: 2, delay_ms: 4000 }));
    expect(event).toEqual({ type: "reconnecting", attempt: 2, delay_ms: 4000 });
  });

  it("parses reconnecting events with defaults for missing fields", () => {
    const event = parseSseEvent(JSON.stringify({ type: "reconnecting" }));
    expect(event).toEqual({ type: "reconnecting", attempt: 1, delay_ms: 2000 });
  });
});

describe("parseSSELines", () => {
  it("splits a chunk on newlines, keeping trailing incomplete line in buffer", () => {
    const buf = { current: "" };
    const lines = parseSSELines("line1\nline2\npartial", buf);
    expect(lines).toEqual(["line1", "line2"]);
    expect(buf.current).toBe("partial");
  });

  it("prepends buffered text to the next chunk", () => {
    const buf = { current: "par" };
    const lines = parseSSELines("tial\ncomplete\n", buf);
    expect(lines).toEqual(["partial", "complete"]);
    expect(buf.current).toBe("");
  });

  it("handles empty chunk with buffer intact", () => {
    const buf = { current: "keep" };
    const lines = parseSSELines("", buf);
    expect(lines).toEqual([]);
    expect(buf.current).toBe("keep");
  });

  it("handles multi-line chunk with no trailing incomplete line", () => {
    const buf = { current: "" };
    const lines = parseSSELines("a\nb\nc\n", buf);
    expect(lines).toEqual(["a", "b", "c"]);
    expect(buf.current).toBe("");
  });

  it("strips carriage returns from CRLF line endings", () => {
    const buf = { current: "" };
    const lines = parseSSELines("data: hello\r\n", buf);
    expect(lines).toEqual(["data: hello"]);
    expect(buf.current).toBe("");
  });

  it("handles chunk with only newlines", () => {
    const buf = { current: "" };
    const lines = parseSSELines("\n\n", buf);
    expect(lines).toEqual(["", ""]);
    expect(buf.current).toBe("");
  });

  it("accumulates cross-chunk partial lines correctly", () => {
    const buf = { current: "" };
    parseSSELines("data: par", buf);
    parseSSELines("tial\ndata: ", buf);
    const lines = parseSSELines("complete\n", buf);
    expect(lines).toEqual(["data: complete"]);
    expect(buf.current).toBe("");
  });
});
