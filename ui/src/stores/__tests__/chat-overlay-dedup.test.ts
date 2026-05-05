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


  it("dedups tool parts already in history by toolCallId across live ChatMessages", () => {
    // After Phase 1 (per-iteration UUIDs), live and history are distinct
    // ChatMessages keyed by row-id. The live ChatMessage is pushed as a new
    // overlay bubble; only its NEW (non-history) tool parts survive.
    const h: ChatMessage[] = [
      msg("u", "user", "hi"),
      { id: "a-h", role: "assistant", parts: [
        { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" },
      ], createdAt: new Date().toISOString() },
    ];
    const live: ChatMessage[] = [
      { id: "a-live", role: "assistant", parts: [
        { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" }, // dup → dropped
        { type: "tool", toolCallId: "t2", toolName: "y", state: "output-available", input: {}, output: "" }, // new → kept
      ], createdAt: new Date().toISOString() },
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(3); // user + history asst + live asst
    const histAsst = result[1];
    expect((histAsst.parts.filter(p => p.type === "tool") as { toolCallId: string }[]).map(t => t.toolCallId)).toEqual(["t1"]);
    const liveAsst = result[2];
    expect((liveAsst.parts.filter(p => p.type === "tool") as { toolCallId: string }[]).map(t => t.toolCallId)).toEqual(["t2"]);
  });

  it("dedups live ChatMessage by mergedIds when convertHistory merged its row", () => {
    // convertHistory merges multiple intermediate iteration rows into one
    // bubble keyed by the FIRST row's id, tracking the rest in mergedIds.
    // A live ChatMessage whose id matches any mergedId must be skipped.
    const h: ChatMessage[] = [
      msg("u", "user", "hi"),
      { id: "iter-0", role: "assistant", parts: [
        { type: "text", text: "intro" },
        { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" },
      ], mergedIds: ["iter-1", "iter-2"], createdAt: new Date().toISOString() },
    ];
    const live: ChatMessage[] = [
      // iter-1 was merged into the bubble above — must NOT show up again
      { id: "iter-1", role: "assistant", parts: [
        { type: "text", text: "intro" },
      ], createdAt: new Date().toISOString() },
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2); // user + history asst, no overlay
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
