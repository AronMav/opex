import { describe, it, expect } from "vitest";
import { historyUpToIncluding } from "../chat-history";
import type { ChatMessage } from "../chat-types";

const msg = (id: string): ChatMessage =>
  ({ id, role: "assistant", parts: [{ type: "text", text: id }], createdAt: "" }) as ChatMessage;

const h = [msg("a"), msg("b"), msg("c")];

describe("historyUpToIncluding — boundary render filter", () => {
  it("cuts history strictly after boundary id", () => {
    expect(historyUpToIncluding(h, "b").map((m) => m.id)).toEqual(["a", "b"]);
  });

  it("includes the boundary message itself (inclusive slice)", () => {
    expect(historyUpToIncluding(h, "a").map((m) => m.id)).toEqual(["a"]);
    expect(historyUpToIncluding(h, "c").map((m) => m.id)).toEqual(["a", "b", "c"]);
  });

  it("boundary id not found → full history (safe)", () => {
    expect(historyUpToIncluding(h, "zzz")).toHaveLength(3);
  });

  it("null boundary → full history", () => {
    expect(historyUpToIncluding(h, null)).toHaveLength(3);
  });
});
