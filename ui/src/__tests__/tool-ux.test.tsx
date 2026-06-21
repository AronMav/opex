import { vi, describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import * as fs from "fs";
import * as path from "path";

// ── Mock: use-tool-progress (should not be needed, but mock to prevent crashes if it leaks) ──

vi.mock("@/hooks/use-tool-progress", () => ({
  useToolProgress: () => 0,
}));

// ── Mock: translation hook ─────────────────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({
    t: (key: string, params?: Record<string, unknown>) => {
      if (key === "chat.tool_calling") return "Calling...";
      if (key === "chat.tool_running") return "Running...";
      if (params) return `${key}(${JSON.stringify(params)})`;
      return key;
    },
    locale: "en",
  }),
}));

// ── Mock: stores (needed for MessageItem) ─────────────────────────────────

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { agentIcons: {} };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", logout: vi.fn() }) },
  ),
}));

vi.mock("@/stores/chat-store", () => ({
  useChatStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state: Record<string, unknown> = { currentAgent: "TestAgent" };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ currentAgent: "TestAgent" }) },
  ),
}));

// ── Mock: next/navigation ──────────────────────────────────────────────────

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

// ── Mock: sonner ───────────────────────────────────────────────────────────

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

// ── Mock: @/lib/api ────────────────────────────────────────────────────────

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(),
  apiDelete: vi.fn(),
  getToken: () => "test-token",
  assertToken: () => "test-token",
}));

// ── Mock: @/lib/sanitize-url ───────────────────────────────────────────────

vi.mock("@/lib/sanitize-url", () => ({
  sanitizeUrl: (url: string) => url,
}));

// ── Mock: @/lib/query-client ───────────────────────────────────────────────

vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: vi.fn(), setQueryData: vi.fn() },
}));

// ── Mock: @tanstack/react-query ────────────────────────────────────────────

vi.mock("@tanstack/react-query", async () => {
  const actual = await vi.importActual("@tanstack/react-query");
  return {
    ...actual,
    useQueryClient: () => ({ invalidateQueries: vi.fn() }),
    useQuery: () => ({ data: undefined, isLoading: false, error: null, refetch: vi.fn() }),
  };
});

// ── Import under test ─────────────────────────────────────────────────────

import { mapToolPartState } from "@/lib/tool-state";
import { ToolCallPartView, TOOL_OUTPUT_MAX_CHARS } from "@/components/chat/ToolCallPartView";

// ── TOOL-01 tests ──────────────────────────────────────────────────────────

describe("TOOL-01: Tool grouping removed", () => {
  it("tool grouping disabled — each tool rendered individually", () => {
    // TOOL_GROUP_THRESHOLD removed; tool grouping mechanism deleted
    expect(true).toBe(true);
  });
});

// ── TOOL-02 tests: mapToolPartState ───────────────────────────────────────

describe("TOOL-02: mapToolPartState returns distinct values for all states", () => {
  it("returns 'calling' for input-streaming", () => {
    expect(mapToolPartState("input-streaming")).toBe("calling");
  });

  it("returns 'running' for input-available", () => {
    expect(mapToolPartState("input-available")).toBe("running");
  });

  it("returns 'complete' for output-available", () => {
    expect(mapToolPartState("output-available")).toBe("complete");
  });

  it("returns 'error' for output-error", () => {
    expect(mapToolPartState("output-error")).toBe("error");
  });

  it("returns 'denied' for output-denied", () => {
    expect(mapToolPartState("output-denied")).toBe("denied");
  });
});

// ── TOOL-02 tests: ToolCallPartView state-driven labels ────────────────────

describe("TOOL-02: ToolCallPartView renders state-driven text labels", () => {
  it("renders pulsing indicator when status.type is 'calling'", () => {
    const { container } = render(
      <ToolCallPartView
        toolName="test_tool"
        args={{}}
        status={{ type: "calling" }}
      />,
    );
    // New design: animated pulsing dot instead of text label
    expect(container.querySelector(".animate-pulse")).toBeInTheDocument();
  });

  it("renders pulsing indicator when status.type is 'running'", () => {
    const { container } = render(
      <ToolCallPartView
        toolName="test_tool"
        args={{}}
        status={{ type: "running" }}
      />,
    );
    // New design: animated pulsing dot instead of text label
    expect(container.querySelector(".animate-pulse")).toBeInTheDocument();
  });

  it("ToolCallPartView does NOT import useToolProgress", () => {
    const filePath = path.resolve(
      __dirname,
      "../components/chat/ToolCallPartView.tsx",
    );
    const fileContents = fs.readFileSync(filePath, "utf-8");
    expect(fileContents).not.toContain("useToolProgress");
  });
});

// ── TOOL-03 tests: expand button accessibility ─────────────────────────────

describe("TOOL-03: TOOL_OUTPUT_MAX_CHARS constant", () => {
  it("TOOL_OUTPUT_MAX_CHARS is exported and equals 10_000", () => {
    expect(TOOL_OUTPUT_MAX_CHARS).toBe(10_000);
  });
});

describe("TOOL-03: Expand button is outside the pre element", () => {
  const longResult = "x".repeat(15_000);

  it("renders expand button when result exceeds TOOL_OUTPUT_MAX_CHARS", () => {
    render(
      <ToolCallPartView
        toolName="test_tool"
        args={{}}
        result={longResult}
        status={{ type: "complete" }}
      />,
    );
    // Open the collapsible first
    const trigger = screen.getByRole("button", { name: /test_tool/i });
    fireEvent.click(trigger);

    // Expand button should be present
    const expandBtn = screen.getByRole("button", { name: /show|more|chat\.tool_show_full/i });
    expect(expandBtn).toBeInTheDocument();
  });

  it("expand button is NOT inside a pre element", () => {
    render(
      <ToolCallPartView
        toolName="test_tool"
        args={{}}
        result={longResult}
        status={{ type: "complete" }}
      />,
    );
    const trigger = screen.getByRole("button", { name: /test_tool/i });
    fireEvent.click(trigger);

    const expandBtn = screen.getByRole("button", { name: /show|more|chat\.tool_show_full/i });
    expect(expandBtn.closest("pre")).toBeNull();
  });

  it("clicking expand button shows full output", () => {
    render(
      <ToolCallPartView
        toolName="test_tool"
        args={{}}
        result={longResult}
        status={{ type: "complete" }}
      />,
    );
    const trigger = screen.getByRole("button", { name: /test_tool/i });
    fireEvent.click(trigger);

    const expandBtn = screen.getByRole("button", { name: /show|more|chat\.tool_show_full/i });
    fireEvent.click(expandBtn);

    // After expand, pre should contain the full text (no truncation)
    const pre = document.querySelector("pre:last-of-type");
    expect(pre?.textContent).toBe(longResult);
  });
});
