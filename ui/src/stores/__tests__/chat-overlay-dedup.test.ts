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

  it("merges consecutive live assistant iterations into one bubble (tool-loop)", () => {
    // History: just the user message
    const h = [msg("u", "user", "что нового?")];
    // Live: user + 3 assistant iterations (tool-call loop)
    const live = [
      msg("u", "user", "что нового?"),
      msg("a1", "assistant", "Загружаю навык."),
      msg("a2", "assistant", "Ищу новости."),
      msg("a3", "assistant", "Дайджест готов."),
    ];
    const result = mergeLiveOverlay(h, live);
    // user already in history (deduplicated) + one merged assistant bubble
    expect(result).toHaveLength(2);
    expect(result[1].role).toBe("assistant");
    expect(result[1].parts).toHaveLength(3); // 3 text parts merged
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
