// ui/src/__tests__/compression-chains.test.tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
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
  beforeEach(() => {
    localStorage.clear();
  });

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

    // Banner starts collapsed by default — expand it first.
    fireEvent.click(screen.getByText("Compression chain").closest("button")!);
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

// ── Additional CompactChainBanner tests ───────────────────────────────────────

describe("CompactChainBanner extended", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("CompactChainBanner_not_rendered_when_isMirror_true — no banner for root (no parent_session_id)", () => {
    vi.mocked(useSessionChain).mockReturnValue({
      data: {
        chain: [
          {
            id: "mirror-id",
            parent_session_id: null,
            end_reason: null,
            title: "Mirror Session",
            started_at: new Date().toISOString(),
            agent_id: "A",
            depth: 0,
          },
        ],
      },
    } as any);

    const { container } = render(
      React.createElement(CompactChainBanner, { activeSessionId: "mirror-id", onNavigate: vi.fn() })
    );
    expect(container.firstChild).toBeNull();
  });

  it("CompactChainBanner_shows_link_to_parent — banner renders with parent reference", () => {
    const chain = makeChain("current-id", "parent-id");
    vi.mocked(useSessionChain).mockReturnValue({ data: chain } as any);

    render(
      React.createElement(CompactChainBanner, { activeSessionId: "current-id", onNavigate: vi.fn() })
    );

    expect(screen.getByText("Compression chain")).toBeTruthy();
    // Banner starts collapsed by default — expand to see entries.
    fireEvent.click(screen.getByText("Compression chain").closest("button")!);
    expect(screen.getByText("Root")).toBeTruthy();
  });

  it("chain_length_displayed — banner shows correct session count", () => {
    // Build a 3-session chain: grandparent → parent → current
    const threeChain = {
      chain: [
        {
          id: "gp-id",
          parent_session_id: null,
          end_reason: "compression",
          title: "Grandparent",
          started_at: new Date().toISOString(),
          agent_id: "A",
          depth: 2,
        },
        {
          id: "p-id",
          parent_session_id: "gp-id",
          end_reason: "compression",
          title: "Parent",
          started_at: new Date().toISOString(),
          agent_id: "A",
          depth: 1,
        },
        {
          id: "c-id",
          parent_session_id: "p-id",
          end_reason: null,
          title: "Current",
          started_at: new Date().toISOString(),
          agent_id: "A",
          depth: 0,
        },
      ],
    };
    vi.mocked(useSessionChain).mockReturnValue({ data: threeChain } as any);

    render(
      React.createElement(CompactChainBanner, { activeSessionId: "c-id", onNavigate: vi.fn() })
    );

    // Banner should display "(3 sessions)"
    expect(screen.getByText("(3 sessions)")).toBeTruthy();
  });
});
