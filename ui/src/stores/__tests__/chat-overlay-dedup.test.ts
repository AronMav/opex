import { describe, it, expect } from "vitest";
import { mergeLiveOverlay, dedupeBubbleTextParts } from "@/stores/chat-overlay-dedup";
import type { ChatMessage, MessagePart } from "@/stores/chat-types";

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
    // Both client and DB now use the same UUID (pre-allocated in sendMessage()).
    // When history refetches mid-stream, historyIds.has(m.id) fires correctly.
    const h = [msg("prealloc-uuid", "user", "да"), msg("a1", "assistant", "ок")];
    const live = [msg("prealloc-uuid", "user", "да")]; // same ID as history
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2); // deduplicated — no duplicate user bubble
  });

  it("optimistic user bubble (sending) stays visible before history catches up", () => {
    const live = [msg("u-opt", "user", "hello")];
    const result = mergeLiveOverlay([], live);
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("u-opt");
  });

  it("optimistic user bubble removed by history when IDs match (stream complete)", () => {
    // After the stream ends, history has both user + assistant messages.
    // Live still has the optimistic user message with the same pre-allocated UUID.
    // ID-based dedup removes the live copy — single user bubble shown.
    const h = [msg("u-pre", "user", "hello"), msg("a-1", "assistant", "reply")];
    const live = [msg("u-pre", "user", "hello")]; // same pre-allocated ID
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2); // deduplicated, not doubled
  });

  it("returns history reference unchanged when there are no extra live messages", () => {
    const h = [msg("1", "user", "hi")];
    const live = [msg("1", "user", "hi")]; // already in history
    const result = mergeLiveOverlay(h, live);
    expect(result).toBe(h); // same reference — no new array
  });

  it("intermediate iterations contribute only their tool parts; only LAST iteration shows text", () => {
    // History: just the user message
    const h = [msg("u", "user", "обсуди с агентами")];
    const live = [
      msg("u", "user", "обсуди с агентами"),
      // iter1 (intermediate): same intro text + tool call
      { id: "a1", role: "assistant" as const, parts: [
        { type: "text" as const, text: "Делегирую задачу." },
        { type: "tool" as const, toolCallId: "tc1", toolName: "agent", state: "output-available" as const, input: {}, output: "" },
      ], createdAt: new Date().toISOString() },
      // iter2 (intermediate): same intro text + another tool
      { id: "a2", role: "assistant" as const, parts: [
        { type: "text" as const, text: "Делегирую задачу." },
        { type: "tool" as const, toolCallId: "tc2", toolName: "search", state: "output-available" as const, input: {}, output: "" },
      ], createdAt: new Date().toISOString() },
      // iter3 (LAST/final): the streaming current iteration — text shown
      { id: "a3", role: "assistant" as const, parts: [
        { type: "text" as const, text: "Делегирую задачу. Готов результат." },
      ], createdAt: new Date().toISOString() },
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
    const parts = result[1].parts;
    const texts = parts.filter(p => p.type === "text");
    const tools = parts.filter(p => p.type === "tool");
    // Only iter3's text appears (iter1+iter2 text dropped); both tools preserved
    expect(texts).toHaveLength(1);
    expect(tools).toHaveLength(2);
    expect((texts[0] as { type: "text"; text: string }).text).toContain("Готов результат");
  });

  it("merges consecutive live assistant iterations: only LAST shows text", () => {
    const h = [msg("u", "user", "вопрос")];
    const live = [
      msg("u", "user", "вопрос"),
      msg("a1", "assistant", "intermediate text 1"),
      msg("a2", "assistant", "intermediate text 2"),
      msg("a3", "assistant", "FINAL text"),
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
    expect(result[1].role).toBe("assistant");
    // Only the last iteration's text remains (intermediate texts dropped)
    expect(result[1].parts).toHaveLength(1);
    expect((result[1].parts[0] as { type: "text"; text: string }).text).toBe("FINAL text");
  });

  it("does NOT continuation-merge into old assistant when history ends with user", () => {
    // RQ cache refreshed quickly: history now has [old_asst, new_user_msg]
    // Live assistants are the RESPONSE to new_user_msg, not a continuation of old_asst
    const h = [
      msg("u1", "user", "старый вопрос"),
      msg("a1", "assistant", "старый ответ"),
      msg("u2", "user", "новый вопрос"),  // ← history ends with user
    ];
    const live = [
      msg("u2", "user", "новый вопрос"), // already in history
      msg("a2", "assistant", "новый ответ iter1"),
      msg("a3", "assistant", "новый ответ iter2"),
    ];
    const result = mergeLiveOverlay(h, live);
    // a2+a3 must appear AFTER u2, not merged into a1 (old turn)
    expect(result).toHaveLength(4); // u1 a1 u2 merged(a2+a3)
    expect(result[3].role).toBe("assistant");
    // old assistant must not be modified
    expect(result[1].parts).toHaveLength(1);
  });

  it("collapses duplicate long text parts within an assistant bubble (final dedup)", () => {
    // Screenshot 2026-05-05: same long intro text appears twice in one bubble
    const longText = "Загружаю навык анализа и получаю данные портфеля. Параллельно подготовлю контекст для мультиагентного анализа.";
    const h: ChatMessage[] = [
      { id: "u", role: "user", parts: [{ type: "text", text: "проанализируй" }], createdAt: new Date().toISOString() },
    ];
    const live: ChatMessage[] = [
      { id: "a", role: "assistant", parts: [
        { type: "text", text: longText },
        { type: "text", text: longText }, // <-- duplicate that previously rendered
        { type: "tool", toolCallId: "t1", toolName: "skill_use", state: "output-available", input: {}, output: "" },
      ], createdAt: new Date().toISOString() },
    ];
    const result = mergeLiveOverlay(h, live);
    const bubble = result.find(m => m.role === "assistant")!;
    const texts = bubble.parts.filter(p => p.type === "text");
    expect(texts).toHaveLength(1);
    expect(bubble.parts.some(p => p.type === "tool")).toBe(true);
  });

  it("dedupeBubbleTextParts: keeps short legitimate repeats below threshold", () => {
    const parts: MessagePart[] = [
      { type: "text", text: "OK" },
      { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" },
      { type: "text", text: "OK" }, // short → kept
    ];
    const result = dedupeBubbleTextParts(parts);
    const texts = result.filter(p => p.type === "text");
    expect(texts).toHaveLength(2);
  });

  it("dedupeBubbleTextParts: returns same reference when nothing dropped", () => {
    const parts: MessagePart[] = [
      { type: "text", text: "Hello world this is unique text content here." },
      { type: "tool", toolCallId: "t1", toolName: "x", state: "output-available", input: {}, output: "" },
    ];
    const result = dedupeBubbleTextParts(parts);
    expect(result).toBe(parts); // same reference — pure pass-through
  });

  it("does NOT merge live assistant across user messages", () => {
    // user1 + asst1 already in history; live adds user2 + asst2
    const h = [msg("u1", "user", "первый"), msg("a1", "assistant", "ответ1")];
    const live = [
      msg("u2", "user", "второй"),
      msg("a2", "assistant", "ответ2"),
    ];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(4); // u1 a1 u2 a2 — not merged across user msg
    expect(result[2].role).toBe("user");
    expect(result[3].role).toBe("assistant");
  });
});
