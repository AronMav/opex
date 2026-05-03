// ui/src/__tests__/compression-chains.test.tsx
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import React from "react";

// ── SessionChainEntry type checks ─────────────────────────────────────────────

describe("SessionRow types", () => {
  it("SessionChainEntry interface compiles with all required fields", () => {
    const entry: import("@/types/api").SessionChainEntry = {
      id: "uuid-a",
      parent_session_id: "uuid-b",
      end_reason: "compression",
      title: "Test Session",
      started_at: new Date().toISOString(),
      agent_id: "TestAgent",
      depth: 0,
    };
    expect(entry.depth).toBe(0);
  });

  it("SessionChainEntry allows null parent_session_id for root", () => {
    const root: import("@/types/api").SessionChainEntry = {
      id: "uuid-root",
      parent_session_id: null,
      end_reason: null,
      title: null,
      started_at: new Date().toISOString(),
      agent_id: "TestAgent",
      depth: 2,
    };
    expect(root.parent_session_id).toBeNull();
  });
});

// ── CompactChainBanner ────────────────────────────────────────────────────────

vi.mock("@/lib/queries", () => ({
  useSessionChain: vi.fn(),
}));

import { useSessionChain } from "@/lib/queries";
import { CompactChainBanner } from "@/components/chat/CompactChainBanner";

function makeChain(currentId: string, parentId: string) {
  return {
    chain: [
      { id: parentId, parent_session_id: null, end_reason: "compression", title: "Root", started_at: new Date().toISOString(), agent_id: "A", depth: 1 },
      { id: currentId, parent_session_id: parentId, end_reason: null, title: "Current", started_at: new Date().toISOString(), agent_id: "A", depth: 0 },
    ],
  };
}

describe("CompactChainBanner", () => {
  it("renders nothing when session has no parent (root session)", () => {
    vi.mocked(useSessionChain).mockReturnValue({
      data: { chain: [{ id: "root", parent_session_id: null, end_reason: null, title: "Root", started_at: new Date().toISOString(), agent_id: "A", depth: 0 }] },
    } as any);

    const { container } = render(
      React.createElement(CompactChainBanner, { activeSessionId: "root", onNavigate: vi.fn() })
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders banner when session has a parent", () => {
    vi.mocked(useSessionChain).mockReturnValue({
      data: makeChain("child-id", "parent-id"),
    } as any);

    render(React.createElement(CompactChainBanner, { activeSessionId: "child-id", onNavigate: vi.fn() }));
    expect(screen.getByText("Compression chain")).toBeTruthy();
  });

  it("calls onNavigate with parent session id on click", () => {
    const onNavigate = vi.fn();
    vi.mocked(useSessionChain).mockReturnValue({
      data: makeChain("child-id", "parent-id"),
    } as any);

    render(React.createElement(CompactChainBanner, { activeSessionId: "child-id", onNavigate }));

    // Banner starts NOT collapsed (localStorage empty in jsdom → collapsed=false).
    // Entries are visible immediately — do NOT click the toggle button first.
    const rootBtn = screen.getByText("Root");
    fireEvent.click(rootBtn);
    expect(onNavigate).toHaveBeenCalledWith("parent-id");
  });
});

// ── ParentBadge ───────────────────────────────────────────────────────────────

import { ParentBadge } from "@/components/chat/ParentBadge";

describe("ParentBadge", () => {
  it("renders parent title in badge", () => {
    const onNavigate = vi.fn();
    render(React.createElement(ParentBadge, { parentTitle: "Original Session", onNavigate }));
    expect(screen.getByText(/Original Session/)).toBeTruthy();
  });

  it("calls onNavigate on click", () => {
    const onNavigate = vi.fn();
    render(React.createElement(ParentBadge, { parentTitle: "Parent", onNavigate }));
    fireEvent.click(screen.getByRole("button"));
    expect(onNavigate).toHaveBeenCalledOnce();
  });
});
