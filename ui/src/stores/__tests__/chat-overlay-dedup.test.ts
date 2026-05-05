import { describe, it, expect } from "vitest";
import { mergeLiveOverlay } from "@/stores/chat-overlay-dedup";
import type { ChatMessage } from "@/stores/chat-types";

function msg(id: string, role: "user" | "assistant", text = ""): ChatMessage {
  return {
    id,
    role,
    parts: text ? [{ type: "text", text }] : [],
    createdAt: new Date().toISOString(),
  };
}

describe("mergeLiveOverlay — pure ID-based dedup", () => {
  it("returns history unchanged when live is empty", () => {
    const h = [msg("1", "user", "hi"), msg("2", "assistant", "hello")];
    expect(mergeLiveOverlay(h, [])).toEqual(h);
  });

  it("appends live messages not yet in history", () => {
    const h = [msg("1", "user", "hi")];
    const live = [msg("2", "assistant", "hello")];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
    expect(result[1].id).toBe("2");
  });

  it("does NOT duplicate messages already in history by ID", () => {
    const h = [msg("1", "user", "hi"), msg("2", "assistant", "hello")];
    const live = [msg("1", "user", "hi"), msg("2", "assistant", "hello")];
    expect(mergeLiveOverlay(h, live)).toHaveLength(2);
  });

  it("filters empty assistant messages from live overlay", () => {
    const h = [msg("1", "user", "hi")];
    const live = [msg("2", "assistant", "")]; // parts is []
    expect(mergeLiveOverlay(h, live)).toHaveLength(1);
  });

  it("optimistic user message deduped when history catches up (same pre-allocated ID)", () => {
    const h = [msg("prealloc-uuid", "user", "да"), msg("a1", "assistant", "ок")];
    const live = [msg("prealloc-uuid", "user", "да")];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
  });

  it("optimistic user bubble (sending) stays visible before history catches up", () => {
    const live = [msg("u-opt", "user", "hello")];
    const result = mergeLiveOverlay([], live);
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("u-opt");
  });

  it("optimistic user bubble removed by history when IDs match (stream complete)", () => {
    const h = [msg("u-pre", "user", "hello"), msg("a-1", "assistant", "reply")];
    const live = [msg("u-pre", "user", "hello")];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
  });

  it("returns history reference unchanged when there are no extra live messages", () => {
    const h = [msg("1", "user", "hi")];
    const live = [msg("1", "user", "hi")];
    const result = mergeLiveOverlay(h, live);
    expect(result).toBe(h);
  });

  it("preserves step-boundary parts from live in the merged bubble", () => {
    // Live message with two iterations separated by a step-boundary
    // (the boundary is inserted by stream-processor on step-start events).
    const h = [msg("u", "user", "hi")];
    const live: ChatMessage[] = [
      msg("u", "user", "hi"),
      { id: "a", role: "assistant", parts: [
        { type: "text", text: "iter1 narration" },
        { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" },
        { type: "step-boundary", stepId: "step_1" },
        { type: "text", text: "iter1 narration" }, // duplicate by content — but LEGAL with boundary
        { type: "tool", toolCallId: "t2", toolName: "y", state: "output-available", input: {}, output: "" },
      ], createdAt: new Date().toISOString() },
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
    const parts = result[1].parts;
    // Both iterations' text remain — duplicates are valid, separated by boundary
    expect(parts.filter(p => p.type === "text")).toHaveLength(2);
    expect(parts.filter(p => p.type === "step-boundary")).toHaveLength(1);
    expect(parts.filter(p => p.type === "tool")).toHaveLength(2);
  });

  it("dedups tool parts already in history by toolCallId (parallel reorder)", () => {
    const h: ChatMessage[] = [
      msg("u", "user", "hi"),
      { id: "a-h", role: "assistant", parts: [
        { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" },
      ], createdAt: new Date().toISOString() },
    ];
    const live: ChatMessage[] = [
      { id: "a-live", role: "assistant", parts: [
        { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" }, // dup
        { type: "tool", toolCallId: "t2", toolName: "y", state: "output-available", input: {}, output: "" }, // new
      ], createdAt: new Date().toISOString() },
    ];
    const result = mergeLiveOverlay(h, live);
    // history asst (1 tool) + live tool t2 merged in via continuation
    const lastAsst = result[result.length - 1];
    expect(lastAsst.role).toBe("assistant");
    const tools = lastAsst.parts.filter(p => p.type === "tool") as { type: "tool"; toolCallId: string }[];
    expect(tools.map(t => t.toolCallId)).toEqual(["t1", "t2"]);
  });

  it("does NOT continuation-merge into old assistant when history ends with user", () => {
    const h = [
      msg("u1", "user", "старый"),
      msg("a1", "assistant", "ответ1"),
      msg("u2", "user", "новый"),
    ];
    const live = [
      msg("u2", "user", "новый"),
      msg("a2", "assistant", "ответ iter1"),
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(4);
    expect(result[1].parts).toHaveLength(1); // old assistant unchanged
    expect(result[3].role).toBe("assistant");
  });

  it("does NOT merge live assistant across user messages", () => {
    const h = [msg("u1", "user", "первый"), msg("a1", "assistant", "ответ1")];
    const live = [
      msg("u2", "user", "второй"),
      msg("a2", "assistant", "ответ2"),
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(4);
    expect(result[2].role).toBe("user");
    expect(result[3].role).toBe("assistant");
  });
});
