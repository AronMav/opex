import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";

// Mock dependencies before importing chat-store
vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: vi.fn(), getQueryData: vi.fn(() => undefined) },
}));
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
  getToken: vi.fn(() => "test-token"),
  assertToken: vi.fn(() => "test-token"),
}));

import {
  convertHistory,
  MAX_INPUT_LENGTH,
  getInitialAgent,
  getLastSessionId,
  saveLastSession,
} from "@/stores/chat-store";
import type { MessageRow } from "@/types/api";

// ── Constants ───────────────────────────────────────────────────────────────

describe("MAX_INPUT_LENGTH", () => {
  it("is 32000", () => {
    expect(MAX_INPUT_LENGTH).toBe(32_000);
  });
});

// ── convertHistory ──────────────────────────────────────────────────────────

function makeRow(overrides: Partial<MessageRow>): MessageRow {
  return {
    id: "m1",
    role: "user",
    content: "",
    tool_calls: null,
    tool_call_id: null,
    created_at: "2026-01-01T00:00:00Z",
    agent_id: null,
    status: "complete",
    feedback: 0,
    edited_at: null,
    thinking_blocks: null,
    parent_message_id: null,
    branch_from_message_id: null,
    abort_reason: null,
    ...overrides,
  };
}

describe("convertHistory", () => {
  it("converts a simple user+assistant exchange", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "Hello" }),
      makeRow({ id: "a1", role: "assistant", content: "Hi there" }),
    ];
    const msgs = convertHistory(rows);
    expect(msgs).toHaveLength(2);
    expect(msgs[0].role).toBe("user");
    expect(msgs[0].parts).toEqual([{ type: "text", text: "Hello" }]);
    expect(msgs[1].role).toBe("assistant");
    expect(msgs[1].parts[0]).toEqual({ type: "text", text: "Hi there" });
  });

  it("always filters out status=streaming rows regardless of isAgentStreaming", () => {
    // convertHistory unconditionally drops rows with status='streaming' (chat-history.ts line 50)
    // — they are transient WAL artefacts. The isAgentStreaming param exists for legacy
    // call-site compatibility but is unused in the function body.
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "Hi" }),
      makeRow({ id: "a1", role: "assistant", content: "partial...", status: "streaming" }),
      makeRow({ id: "a2", role: "assistant", content: "Full response", status: "complete" }),
    ];
    const msgsWithFlag = convertHistory(rows, true);
    const msgsWithout = convertHistory(rows);
    // Both calls: streaming row dropped, only the complete row remains
    expect(msgsWithFlag.filter(m => m.role === "assistant")).toHaveLength(1);
    expect(msgsWithFlag.filter(m => m.role === "assistant")[0].parts[0]).toEqual({ type: "text", text: "Full response" });
    expect(msgsWithout.filter(m => m.role === "assistant")).toHaveLength(1);
    expect(msgsWithout.filter(m => m.role === "assistant")[0].parts[0]).toEqual({ type: "text", text: "Full response" });
  });

  it("extracts <think> blocks as reasoning parts", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "question" }),
      makeRow({
        id: "a1",
        role: "assistant",
        content: "<think>Let me think...</think>The answer is 42.",
      }),
    ];
    const msgs = convertHistory(rows);
    const parts = msgs[1].parts;
    expect(parts).toHaveLength(2);
    expect(parts[0]).toEqual({ type: "reasoning", text: "Let me think..." });
    expect(parts[1]).toEqual({ type: "text", text: "The answer is 42." });
  });

  it("handles tool call lifecycle (assistant+tool rows)", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "search for cats" }),
      makeRow({
        id: "a1",
        role: "assistant",
        content: "",
        tool_calls: [{ id: "tc1", name: "search", arguments: { q: "cats" } }],
      }),
      makeRow({
        id: "t1",
        role: "tool",
        content: "Found 5 results",
        tool_call_id: "tc1",
      }),
      makeRow({ id: "a2", role: "assistant", content: "Here are your results." }),
    ];
    const msgs = convertHistory(rows);
    // user + assistant(tool) + assistant(text) = 3 messages
    expect(msgs).toHaveLength(3);
    const toolPart = msgs.flatMap(m => m.parts).find(p => p.type === "tool");
    expect(toolPart?.type).toBe("tool");
    if (toolPart?.type === "tool") {
      expect(toolPart.toolName).toBe("search");
      expect(toolPart.state).toBe("output-available");
      expect(toolPart.output).toBe("Found 5 results");
    }
  });

  it("extracts __file__ markers from tool output", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "show image" }),
      makeRow({
        id: "a1",
        role: "assistant",
        content: "",
        tool_calls: [{ id: "tc1", name: "img", arguments: {} }],
      }),
      makeRow({
        id: "t1",
        role: "tool",
        content: '__file__:{"url":"/img.png","mediaType":"image/png"}\nDone',
        tool_call_id: "tc1",
      }),
    ];
    const msgs = convertHistory(rows);
    const parts = msgs.flatMap(m => m.parts);
    const filePart = parts.find(p => p.type === "file");
    expect(filePart).toEqual({ type: "file", url: "/img.png", mediaType: "image/png" });
    const toolPart = parts.find(p => p.type === "tool");
    if (toolPart?.type === "tool") {
      expect(toolPart.output).toBe("Done");
    }
  });

  it("returns empty array for empty input", () => {
    expect(convertHistory([])).toEqual([]);
  });

  it("preserves agentId from rows", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "hi", agent_id: "Agent1" }),
    ];
    const msgs = convertHistory(rows);
    expect(msgs[0].agentId).toBe("Agent1");
  });
});

// ── STATE-03: convertHistory agentId forward-fill ───────────────────────────

describe("STATE-03: convertHistory agentId forward-fill", () => {
  it("forward-fills agentId from last seen non-null agent_id", () => {
    // rows: user(AgentA), assistant(AgentA), tool(null), assistant(null)
    // D-01: No merge — 2 separate assistant messages
    const rows: MessageRow[] = [
      makeRow({ id: "u1", role: "user", content: "hi", agent_id: "AgentA" }),
      makeRow({
        id: "a1",
        role: "assistant",
        content: "",
        agent_id: "AgentA",
        tool_calls: [{ id: "tc1", name: "search", arguments: {} }],
      }),
      makeRow({ id: "t1", role: "tool", content: "result", tool_call_id: "tc1", agent_id: null }),
      makeRow({ id: "a2", role: "assistant", content: "Done", agent_id: null }),
    ];
    const msgs = convertHistory(rows);
    const assistantMsgs = msgs.filter(m => m.role === "assistant");
    // D-01: Each assistant row = separate message; forward-fill gives AgentA to second
    expect(assistantMsgs).toHaveLength(2);
    expect(assistantMsgs[0].agentId).toBe("AgentA");
    expect(assistantMsgs[1].agentId).toBe("AgentA"); // forward-filled
  });

  it("does not forward-fill agentId when a new agent_id appears", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "a1", role: "assistant", content: "First", agent_id: "AgentA" }),
      makeRow({ id: "a2", role: "assistant", content: "Second", agent_id: "AgentB" }),
    ];
    const msgs = convertHistory(rows);
    const assistantMsgs = msgs.filter(m => m.role === "assistant");
    expect(assistantMsgs[0].agentId).toBe("AgentA");
    expect(assistantMsgs[1].agentId).toBe("AgentB");
  });

  it("leaves agentId undefined when no prior agent_id exists", () => {
    const rows: MessageRow[] = [
      makeRow({ id: "a1", role: "assistant", content: "Solo", agent_id: null }),
    ];
    const msgs = convertHistory(rows);
    expect(msgs[0].agentId).toBeUndefined();
  });
});

// ── localStorage helpers ────────────────────────────────────────────────────

describe("getInitialAgent", () => {
  it("returns first agent when nothing saved", () => {
    localStorage.removeItem("hydeclaw.lastSession");
    expect(getInitialAgent(["A", "B"])).toBe("A");
  });

  it("returns empty string for empty list", () => {
    expect(getInitialAgent([])).toBe("");
  });
});

describe("saveLastSession / getLastSessionId", () => {
  it("saves and retrieves session id per agent", () => {
    saveLastSession("Agent1", "sess-1");
    expect(getLastSessionId("Agent1")).toBe("sess-1");
  });

  it("returns undefined for unknown agent", () => {
    localStorage.removeItem("hydeclaw.lastSession");
    expect(getLastSessionId("Unknown")).toBeUndefined();
  });
});

// ── STATE-01: history to live transition ────────────────────────────────────

describe("STATE-01: history to live transition", () => {
  beforeEach(async () => {
    const { useChatStore } = await import("@/stores/chat-store");
    useChatStore.setState({ agents: {}, currentAgent: "" });
  });

  afterEach(async () => {
    const { useChatStore } = await import("@/stores/chat-store");
    useChatStore.setState({ agents: {}, currentAgent: "" });
  });

  it("sendMessage from history mode seeds messageSource atomically (no empty live transition)", async () => {
    const { useChatStore } = await import("@/stores/chat-store");

    // Set up agent in history mode with an active session
    useChatStore.setState((s) => {
      s.currentAgent = "TestAgent";
      if (!s.agents["TestAgent"]) {
        s.agents["TestAgent"] = {
          activeSessionId: "sess-history",
          messageSource: { mode: "history", sessionId: "sess-history" },
          streamError: null,
          connectionPhase: "idle",
          connectionError: null,
          forceNewSession: false,
          activeSessionIds: [],
          renderLimit: 100,
          modelOverride: null,
          turnLimitMessage: null,
          streamGeneration: 0,
          reconnectAttempt: 0,
          maxReconnectAttempts: 3,
          isLlmReconnecting: false,
          selectedBranches: {},
        };
      } else {
        s.agents["TestAgent"].messageSource = { mode: "history", sessionId: "sess-history" };
        s.agents["TestAgent"].activeSessionId = "sess-history";
        s.agents["TestAgent"].connectionPhase = "idle";
      }
    });

    // Record all states during sendMessage
    type MessageSource = { mode: "new-chat" } | { mode: "live"; messages: unknown[] } | { mode: "history"; sessionId: string };
    const stateSnapshots: Array<{ messageSource: MessageSource }> = [];
    const unsub = useChatStore.subscribe((state) => {
      const ag = state.agents["TestAgent"];
      if (ag) {
        stateSnapshots.push({ messageSource: ag.messageSource as MessageSource });
      }
    });

    // Mock fetch to prevent actual network calls
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(new ReadableStream(), { status: 200 })
    );

    useChatStore.getState().sendMessage("hello");

    unsub();
    fetchSpy.mockRestore();

    // After sendMessage, every transition to "live" messageSource must have non-empty messages.
    // (startStream atomically sets { mode: "live", messages: [...seedMessages, userMsg] })
    const liveTransitions = stateSnapshots.filter((s) => s.messageSource.mode === "live");
    for (const snap of liveTransitions) {
      if (snap.messageSource.mode === "live") {
        expect(snap.messageSource.messages.length).toBeGreaterThan(0);
      }
    }
  });

  it("chat-store.ts sendMessage uses messageSource (no early viewMode flip)", async () => {
    // Static analysis: verify the store uses messageSource.mode checks, not viewMode checks.
    // sendMessage / stopStream / regenerate / regenerateFrom live in stream-control.ts after
    // the chat-store modularisation (Tasks 1.8-1.10).
    const fs = await import("node:fs");
    const path = await import("node:path");
    const src = fs.readFileSync(
      path.resolve(__dirname, "../stores/chat/actions/stream-control.ts"),
      "utf8"
    );

    // sendMessage block — from "sendMessage: (text: string) => {" to "stopStream:"
    // Note: skip over the interface declaration which uses "=> void;" not "=> {"
    const sendMsgImplStart = src.indexOf("sendMessage: (text");
    // Find the opening brace of the implementation
    const sendMsgBrace = src.indexOf("=> {", sendMsgImplStart);
    const sendMessageBlock = src.slice(
      sendMsgBrace,
      src.indexOf("stopStream:", sendMsgBrace)
    );
    // No old-style viewMode update calls
    expect(sendMessageBlock).not.toMatch(/update\(agent,\s*\{\s*viewMode:\s*["']live["']\s*\}/);
    // Uses messageSource.mode checks
    expect(sendMessageBlock).toMatch(/messageSource\.mode/);

    // regenerate block — from "regenerate: () =>" to "regenerateFrom:"
    const regenerateBlock = src.slice(
      src.indexOf("regenerate: ()"),
      src.indexOf("regenerateFrom:")
    );
    expect(regenerateBlock).not.toMatch(/update\(agent,\s*\{\s*viewMode:\s*["']live["']\s*\}/);

    // regenerateFrom block — from "regenerateFrom: " to "forkAndRegenerate:"
    // (renameSession moved to session-crud.ts; forkAndRegenerate is the next action in stream-control.ts)
    const regenerateFromStart = src.indexOf("regenerateFrom: (messageId");
    const regenerateFromEnd = src.indexOf("forkAndRegenerate:", regenerateFromStart);
    const regenerateFromBlock = src.slice(regenerateFromStart, regenerateFromEnd);
    expect(regenerateFromBlock).not.toMatch(/update\(agent,\s*\{\s*viewMode:\s*["']live["']\s*\}/);
  });
});

// ── STATE-02: beforeunload flush (removed from chat-store) ──────────────────

describe("STATE-02: beforeunload removed", () => {
  it("chat-store.ts no longer registers beforeunload on window", async () => {
    const fs = await import("node:fs");
    const path = await import("node:path");
    const src = fs.readFileSync(
      path.resolve(__dirname, "../stores/chat-store.ts"),
      "utf8"
    );
    // beforeunload and keepalive were removed from chat-store as part of cleanup
    expect(src).not.toContain("beforeunload");
    expect(src).not.toContain("keepalive: true");
  });
});
