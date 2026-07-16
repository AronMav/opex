import { describe, it, expect } from "vitest";
import { mergeRender } from "../chat-selectors";
import type { ChatMessage } from "../chat-types";

// mergeRender is the id-keyed render merge that replaced the positional
// boundary-slice model. Contract:
//  - history order is preserved;
//  - for an id present in BOTH history and live, the LIVE message wins;
//  - live-only messages append after history, in their original order.

const user = (id: string, text: string): ChatMessage => ({
  id,
  role: "user",
  parts: [{ type: "text", text }],
  createdAt: "",
});

const assistant = (id: string, text: string): ChatMessage => ({
  id,
  role: "assistant",
  parts: [{ type: "text", text }],
  createdAt: "",
});

function texts(msgs: ChatMessage[]): string[] {
  return msgs.map((m) =>
    m.parts.flatMap((p) => (p.type === "text" ? [p.text] : [])).join(""),
  );
}

describe("mergeRender", () => {
  it("appends live-only messages after history (history untouched)", () => {
    const history = [user("u0", "first"), assistant("a0", "reply one")];
    const live = [user("b", "second"), assistant("a1", "reply two")];
    const out = mergeRender(history, live);
    expect(out.map((m) => m.id)).toEqual(["u0", "a0", "b", "a1"]);
  });

  it("dedups a shared id to a single row — LIVE wins", () => {
    // history has a stale/partial row under id "b"; live carries the fresh one.
    const history = [user("u0", "first"), user("b", "STALE from history")];
    const live = [user("b", "FRESH from live"), assistant("a1", "reply")];
    const out = mergeRender(history, live);
    expect(out.map((m) => m.id)).toEqual(["u0", "b", "a1"]);
    // "b" appears exactly once, and it is the live copy.
    expect(out.filter((m) => m.id === "b")).toHaveLength(1);
    expect(texts(out)).toEqual(["first", "FRESH from live", "reply"]);
  });

  it("live wins for a partial assistant that history has under the same id", () => {
    const history = [user("u1", "q"), assistant("X", "partial")];
    const live = [assistant("X", "partial + much more")];
    const out = mergeRender(history, live);
    expect(out.map((m) => m.id)).toEqual(["u1", "X"]);
    expect(texts(out)).toEqual(["q", "partial + much more"]);
  });

  it("preserves history order even when live is empty", () => {
    const history = [user("u0", "a"), assistant("a0", "b"), user("u1", "c")];
    expect(mergeRender(history, []).map((m) => m.id)).toEqual(["u0", "a0", "u1"]);
  });

  it("does not double-render a duplicate id within the live array (R1 nit — seen.add)", () => {
    // A (theoretical) duplicate id inside `live` must collapse to one row.
    const history = [user("u0", "first")];
    const live = [assistant("dup", "one"), assistant("dup", "two")];
    const out = mergeRender(history, live);
    expect(out.filter((m) => m.id === "dup")).toHaveLength(1);
    expect(out.map((m) => m.id)).toEqual(["u0", "dup"]);
  });

  it("is idempotent on re-apply (merging its own output with live is stable)", () => {
    const history = [user("u0", "first"), assistant("a0", "reply one")];
    const live = [user("b", "second"), assistant("a1", "reply two")];
    const once = mergeRender(history, live);
    const twice = mergeRender(once, live);
    expect(twice.map((m) => m.id)).toEqual(once.map((m) => m.id));
    expect(twice.filter((m) => m.id === "b")).toHaveLength(1);
    expect(twice.filter((m) => m.id === "a1")).toHaveLength(1);
  });
});
