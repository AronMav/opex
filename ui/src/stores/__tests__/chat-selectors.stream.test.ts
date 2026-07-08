import { describe, it, expect } from "vitest";
import { selectLiveAssistantText } from "@/stores/chat-selectors";
import type { ChatState } from "@/stores/chat-types";

function stateWith(mode: string, messages: unknown[]): ChatState {
  return { agents: { A: { messageSource: { mode, messages } } } } as unknown as ChatState;
}

describe("selectLiveAssistantText", () => {
  it("returns the last assistant message's concatenated text in live mode", () => {
    const state = stateWith("live", [
      { id: "u", role: "user", parts: [{ type: "text", text: "hi" }] },
      { id: "a1", role: "assistant", parts: [{ type: "text", text: "Hello " }, { type: "text", text: "world." }] },
    ]);
    expect(selectLiveAssistantText(state, "A")).toEqual({ id: "a1", text: "Hello world." });
  });

  it("ignores non-text parts", () => {
    const state = stateWith("live", [
      { id: "a1", role: "assistant", parts: [{ type: "tool", toolName: "x" }, { type: "text", text: "ok" }] },
    ]);
    expect(selectLiveAssistantText(state, "A")).toEqual({ id: "a1", text: "ok" });
  });

  it("returns empty when not live/finishing", () => {
    const state = { agents: { A: { messageSource: { mode: "history", sessionId: "s" } } } } as unknown as ChatState;
    expect(selectLiveAssistantText(state, "A")).toEqual({ id: "", text: "" });
  });

  it("returns empty when the agent is unknown", () => {
    expect(selectLiveAssistantText({ agents: {} } as unknown as ChatState, "A")).toEqual({ id: "", text: "" });
  });
});
