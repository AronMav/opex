// Unit tests for searchMessages() in chat-history.ts
// Testing: empty query, no matches, multi-match per message, case-insensitive, non-text parts ignored.

import { describe, it, expect } from "vitest";
import { searchMessages } from "@/stores/chat-history";
import type { ChatMessage } from "@/stores/chat-types";

function makeMsg(id: string, role: "user" | "assistant", ...texts: string[]): ChatMessage {
  return {
    id,
    role,
    parts: texts.map((t) => ({ type: "text" as const, text: t })),
  };
}

function makeMsgMixed(id: string): ChatMessage {
  return {
    id,
    role: "assistant",
    parts: [
      { type: "text" as const, text: "Hello world" },
      { type: "tool" as const, toolCallId: "tc1", toolName: "search", state: "output-available", input: {}, output: "result" },
      { type: "file" as const, url: "/uploads/file.png", mediaType: "image/png" },
      { type: "text" as const, text: "world is big" },
    ],
  };
}

describe("searchMessages", () => {
  it("returns empty array for empty query", () => {
    const msgs = [makeMsg("1", "user", "hello world")];
    expect(searchMessages("", msgs)).toEqual([]);
  });

  it("returns empty array when no messages", () => {
    expect(searchMessages("hello", [])).toEqual([]);
  });

  it("returns empty array when no matches", () => {
    const msgs = [makeMsg("1", "user", "hello world")];
    expect(searchMessages("xyz", msgs)).toEqual([]);
  });

  it("finds a single match in a message", () => {
    const msgs = [makeMsg("1", "user", "hello world")];
    const result = searchMessages("hello", msgs);
    expect(result).toHaveLength(1);
    expect(result[0].messageId).toBe("1");
    expect(result[0].partIndex).toBe(0);
    expect(result[0].ranges).toEqual([{ start: 0, end: 5 }]);
  });

  it("finds multiple occurrences in a single text part", () => {
    const msgs = [makeMsg("1", "user", "foo bar foo baz foo")];
    const result = searchMessages("foo", msgs);
    expect(result).toHaveLength(1);
    expect(result[0].ranges).toHaveLength(3);
    expect(result[0].ranges[0]).toEqual({ start: 0, end: 3 });
    expect(result[0].ranges[1]).toEqual({ start: 8, end: 11 });
    expect(result[0].ranges[2]).toEqual({ start: 16, end: 19 });
  });

  it("is case-insensitive", () => {
    const msgs = [makeMsg("1", "user", "Hello WORLD hello")];
    const result = searchMessages("hello", msgs);
    expect(result).toHaveLength(1);
    expect(result[0].ranges).toHaveLength(2);
  });

  it("matches across multiple messages", () => {
    const msgs = [
      makeMsg("1", "user", "first match here"),
      makeMsg("2", "assistant", "no hit"),
      makeMsg("3", "user", "second match here"),
    ];
    const result = searchMessages("match", msgs);
    expect(result).toHaveLength(2);
    expect(result[0].messageId).toBe("1");
    expect(result[1].messageId).toBe("3");
  });

  it("skips non-text parts (tool, file)", () => {
    const msgs = [makeMsgMixed("1")];
    const result = searchMessages("world", msgs);
    // Should only match text parts at partIndex 0 and 3
    expect(result).toHaveLength(2);
    expect(result[0].partIndex).toBe(0); // "Hello world"
    expect(result[1].partIndex).toBe(3); // "world is big"
  });

  it("returns correct ranges for multi-part match", () => {
    const msgs = [makeMsgMixed("1")];
    const result = searchMessages("world", msgs);
    expect(result[0].ranges).toEqual([{ start: 6, end: 11 }]); // "Hello world"
    expect(result[1].ranges).toEqual([{ start: 0, end: 5 }]);  // "world is big"
  });
});
