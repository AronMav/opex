import { describe, it, expect } from "vitest";
import { sessionToMarkdown } from "@/lib/format";
import type { ChatMessage } from "@/stores/chat-store";
import type { SessionRow } from "@/types/api";

// Minimal session mock matching the real SessionRow type
const mockSession: SessionRow = {
  id: "sess-abc-123",
  agent_id: "main",
  user_id: "user-1",
  channel: "web",
  chat_scope: null,
  started_at: "2024-01-15T10:30:00.000Z",
  last_message_at: "2024-01-15T10:35:00.000Z",
  title: null,
  run_status: null,
  metadata: null,
  participants: [],
  parent_session_id: null,
  end_reason: null,
};

// Minimal ChatMessage mocks matching the real ChatMessage type
const userMessage: ChatMessage = {
  id: "msg-1",
  role: "user",
  parts: [{ type: "text", text: "Привет, как дела?" }],
  createdAt: "2024-01-15T10:30:00.000Z",
};

const assistantMessage: ChatMessage = {
  id: "msg-2",
  role: "assistant",
  parts: [{ type: "text", text: "Всё отлично, спасибо!" }],
  createdAt: "2024-01-15T10:30:05.000Z",
};

const assistantWithTool: ChatMessage = {
  id: "msg-3",
  role: "assistant",
  parts: [
    {
      type: "tool",
      toolCallId: "tc-1",
      toolName: "search_web",
      state: "output-available",
      input: { query: "test" },
      output: "Search results here",
    },
    { type: "text", text: "Нашёл результаты." },
  ],
  createdAt: "2024-01-15T10:31:00.000Z",
};

describe("sessionToMarkdown", () => {
  it("includes agent name", () => {
    const result = sessionToMarkdown([userMessage, assistantMessage], mockSession, "Agent1");
    expect(result).toContain("Agent1");
  });

  it("includes session id", () => {
    const result = sessionToMarkdown([userMessage, assistantMessage], mockSession, "Agent1");
    expect(result).toContain("sess-abc-123");
  });

  it("renders user message", () => {
    const result = sessionToMarkdown([userMessage], mockSession, "Agent1");
    expect(result).toContain("Привет, как дела?");
    expect(result).toContain("**You**");
  });

  it("renders assistant message", () => {
    const result = sessionToMarkdown([assistantMessage], mockSession, "Agent1");
    expect(result).toContain("Всё отлично, спасибо!");
    expect(result).toContain("**Agent1**");
  });

  it("does not crash on empty messages array", () => {
    const result = sessionToMarkdown([], mockSession, "Agent1");
    expect(result).toContain("# Session Export");
  });
});
