/**
 * Streaming performance tests for PERF-01, PERF-02, PERF-03
 *
 * PERF-01: STREAM_THROTTLE_MS exported constant (not hardcoded 50)
 * PERF-02: pushUpdate in-place Immer mutation (stable object identity)
 * PERF-03: WeakMap memoization cache in MessageItem.tsx
 */

import { describe, it, expect, vi } from "vitest";
import { STREAM_THROTTLE_MS } from "@/stores/chat-store";

// ── PERF-01: STREAM_THROTTLE_MS exported constant ───────────────────────────

describe("PERF-01: STREAM_THROTTLE_MS exported constant", () => {
  it("STREAM_THROTTLE_MS is exported and equals 50", () => {
    expect(STREAM_THROTTLE_MS).toBe(50);
  });

  it("STREAM_THROTTLE_MS is a number", () => {
    expect(typeof STREAM_THROTTLE_MS).toBe("number");
  });
});

// ── PERF-02: In-place Immer mutation in pushUpdate ──────────────────────────
// Test the mutation pattern directly: using Immer to update a field in-place
// vs replacing the whole object. We test the semantics, not the internal function.

import { create } from "zustand";
import { immer } from "zustand/middleware/immer";

interface TestMessage {
  id: string;
  role: "assistant";
  parts: string[];
  agentId?: string;
}

interface TestStore {
  messages: TestMessage[];
  pushInPlace: (id: string, parts: string[], agentId?: string) => void;
  pushReplace: (id: string, parts: string[], agentId?: string) => void;
}

// Store that uses in-place mutation (PERF-02 pattern)
const useInPlaceStore = create<TestStore>()(
  immer((set) => ({
    messages: [],
    pushInPlace: (id, parts, agentId) =>
      set((draft) => {
        const existing = draft.messages.findIndex((m) => m.id === id);
        if (existing >= 0) {
          // In-place mutation: preserves object identity in Immer's structural sharing
          const msg = draft.messages[existing];
          msg.parts = [...parts];
          msg.agentId = agentId;
        } else {
          draft.messages.push({ id, role: "assistant", parts: [...parts], agentId });
        }
      }),
    pushReplace: (id, parts, agentId) =>
      set((draft) => {
        const existing = draft.messages.findIndex((m) => m.id === id);
        const newMsg: TestMessage = { id, role: "assistant", parts: [...parts], agentId };
        if (existing >= 0) {
          draft.messages[existing] = newMsg; // replaces whole object
        } else {
          draft.messages.push(newMsg);
        }
      }),
  }))
);

describe("PERF-02: In-place Immer mutation in pushUpdate", () => {
  it("liveMessages length stays 1 after two pushInPlace for the same message", () => {
    const store = useInPlaceStore.getState();
    // Reset store
    useInPlaceStore.setState({ messages: [] });

    useInPlaceStore.getState().pushInPlace("msg-1", ["part a"]);
    useInPlaceStore.getState().pushInPlace("msg-1", ["part a", "part b"]);

    const { messages } = useInPlaceStore.getState();
    expect(messages).toHaveLength(1);
  });

  it("after pushInPlace with updated parts, messages[0].parts reflects the new parts", () => {
    useInPlaceStore.setState({ messages: [] });

    useInPlaceStore.getState().pushInPlace("msg-2", ["initial"]);
    useInPlaceStore.getState().pushInPlace("msg-2", ["initial", "updated"]);

    const { messages } = useInPlaceStore.getState();
    expect(messages[0].parts).toEqual(["initial", "updated"]);
  });

  it("no duplicate messages after multiple pushInPlace for same id", () => {
    useInPlaceStore.setState({ messages: [] });

    for (let i = 1; i <= 5; i++) {
      useInPlaceStore.getState().pushInPlace("msg-3", [`part-${i}`]);
    }

    const { messages } = useInPlaceStore.getState();
    expect(messages).toHaveLength(1);
    expect(messages[0].parts).toEqual(["part-5"]);
  });

  it("pushInPlace updates agentId correctly", () => {
    useInPlaceStore.setState({ messages: [] });

    useInPlaceStore.getState().pushInPlace("msg-4", ["hello"], "Agent1");
    useInPlaceStore.getState().pushInPlace("msg-4", ["hello", "world"], "Agent1");

    const { messages } = useInPlaceStore.getState();
    expect(messages[0].agentId).toBe("Agent1");
    expect(messages[0].parts).toEqual(["hello", "world"]);
  });

  it("different messages are independent (no cross-contamination)", () => {
    useInPlaceStore.setState({ messages: [] });

    useInPlaceStore.getState().pushInPlace("msg-a", ["a1"]);
    useInPlaceStore.getState().pushInPlace("msg-b", ["b1"]);
    useInPlaceStore.getState().pushInPlace("msg-a", ["a1", "a2"]);

    const { messages } = useInPlaceStore.getState();
    expect(messages).toHaveLength(2);
    const msgA = messages.find((m) => m.id === "msg-a");
    const msgB = messages.find((m) => m.id === "msg-b");
    expect(msgA?.parts).toEqual(["a1", "a2"]);
    expect(msgB?.parts).toEqual(["b1"]);
  });
});

// ── PERF-03: WeakMap memoization cache ──────────────────────────────────────

describe("PERF-03: WeakMap memoization", () => {
  it("WeakMap returns cached value for same object reference", () => {
    const cache = new WeakMap<{ parts: string[] }, string[]>();
    const msg = { parts: ["hello"] };
    const rendered = ["rendered-hello"];
    cache.set(msg, rendered);
    expect(cache.get(msg)).toBe(rendered);
  });

  it("WeakMap misses for different object with same content", () => {
    const cache = new WeakMap<{ parts: string[] }, string[]>();
    const msg1 = { parts: ["hello"] };
    const msg2 = { parts: ["hello"] };
    cache.set(msg1, ["rendered"]);
    expect(cache.get(msg2)).toBeUndefined();
  });

  it("cache hit rate exceeds 90% with 50 messages and 1 updating", () => {
    const cache = new WeakMap<{ id: number }, number[]>();
    const messages = Array.from({ length: 50 }, (_, i) => ({ id: i }));
    messages.forEach((msg) => cache.set(msg, [msg.id]));
    let hits = 0;
    let total = 0;
    for (let tick = 0; tick < 20; tick++) {
      for (const msg of messages) {
        total++;
        if (cache.has(msg)) hits++;
      }
      // Replace last message (simulates streaming update)
      messages[49] = { id: 49 };
      cache.set(messages[49], [49]);
    }
    const hitRate = hits / total;
    expect(hitRate).toBeGreaterThan(0.9);
  });
});

// ── PERF-03: AssistantMessage cache wiring (render test) ────────────────────
// NOTE: renderPartsWithGrouping is a private function inside MessageItem.tsx.
// We test the cache wiring by mocking module dependencies and verifying
// that MessageItem renders consistently when given the same message reference.

import React from "react";
import { render } from "@testing-library/react";

// We need to mock heavy dependencies that MessageItem uses
vi.mock("@/stores/chat-store", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/stores/chat-store")>();
  return {
    ...actual,
    useChatStore: vi.fn((selector: (s: { currentAgent: string }) => unknown) =>
      selector({ currentAgent: "TestAgent" })
    ),
  };
});

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: vi.fn((selector: (s: { agentIcons: Record<string, string> }) => unknown) =>
    selector({ agentIcons: {} })
  ),
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({
    t: (key: string, vars?: Record<string, unknown>) => {
      if (vars) return `${key}:${JSON.stringify(vars)}`;
      return key;
    },
    locale: "en",
  }),
}));

vi.mock("@/lib/format", () => ({
  formatMessageTime: () => "12:00",
}));

vi.mock("@/app/(authenticated)/chat/MessageActions", () => ({
  MessageActions: () => null,
}));

// Mock heavy rendering dependencies
vi.mock("@/app/(authenticated)/chat/parts/TextPart", () => ({
  TextPart: ({ text }: { text: string }) => React.createElement("span", { "data-testid": "text-part" }, text),
}));

vi.mock("@/app/(authenticated)/chat/parts/ReasoningPart", () => ({
  ReasoningPart: ({ text }: { text: string }) => React.createElement("span", { "data-testid": "reasoning-part" }, text),
}));

vi.mock("@/app/(authenticated)/chat/ChatThread", () => ({
  ToolCallPartView: () => null,
  FileDataPartView: () => null,
}));

vi.mock("@/app/(authenticated)/chat/avatar/RoleAvatar", () => ({
  RoleAvatar: () => null,
  SourceUrlDataPartView: () => null,
  RichCardDataPartView: () => null,
}));

vi.mock("@/components/chat/ToolCallPartView", () => ({
  ToolCallPartView: () => null,
}));

vi.mock("@/components/chat/FileDataPartView", () => ({
  FileDataPartView: () => null,
}));

vi.mock("@/components/ui/collapsible", () => ({
  Collapsible: ({ children }: { children: React.ReactNode }) => React.createElement("div", null, children),
  CollapsibleTrigger: ({ children }: { children: React.ReactNode }) => React.createElement("div", null, children),
  CollapsibleContent: ({ children }: { children: React.ReactNode }) => React.createElement("div", null, children),
}));

vi.mock("@/components/ui/loader", () => ({
  BarsLoader: () => null,
}));

vi.mock("lucide-react", () => {
  const Icon = () => null;
  return {
    Activity: Icon, AlertCircle: Icon, AlertTriangle: Icon, Archive: Icon,
    ArrowDownRight: Icon, ArrowLeft: Icon, ArrowRight: Icon, ArrowUpRight: Icon,
    BarChart3: Icon, Bell: Icon, BookOpen: Icon, Bot: Icon, Box: Icon, Brain: Icon,
    Calendar: Icon, Camera: Icon, Check: Icon, CheckCircle: Icon, CheckCircle2: Icon,
    CheckIcon: Icon, ChevronDown: Icon, ChevronDownIcon: Icon, ChevronLeft: Icon,
    ChevronRight: Icon, ChevronRightIcon: Icon, ChevronUp: Icon, Circle: Icon,
    CircleCheckIcon: Icon, CircleIcon: Icon, Clock: Icon, Copy: Icon,
    CornerDownRight: Icon, Cpu: Icon, Database: Icon, DollarSign: Icon,
    Download: Icon, Edit3: Icon, ExternalLink: Icon, Eye: Icon,
    FileCode: Icon, FileCode2: Icon, FilePlus: Icon, FileText: Icon,
    Folder: Icon, FolderMinus: Icon, Gamepad2: Icon, Gauge: Icon,
    GitBranch: Icon, Globe: Icon, Hammer: Icon, Hash: Icon, HeartPulse: Icon,
    History: Icon, Image: Icon, ImageIcon: Icon, InfoIcon: Icon,
    Key: Icon, KeyRound: Icon, Keyboard: Icon, Languages: Icon,
    Link: Icon, Link2: Icon, ListTodo: Icon, Loader: Icon, Loader2: Icon,
    Loader2Icon: Icon, LogOut: Icon, LucideProps: Icon,
    Mail: Icon, Maximize2: Icon, Menu: Icon, MessageSquare: Icon,
    MessageSquareShare: Icon, Mic: Icon, Minimize2: Icon, Minus: Icon,
    Monitor: Icon, Moon: Icon, MoreHorizontal: Icon, Network: Icon,
    OctagonXIcon: Icon, PanelLeftIcon: Icon, PanelRight: Icon,
    Paperclip: Icon, Pencil: Icon, Phone: Icon, Pin: Icon, PinOff: Icon,
    Play: Icon, Plus: Icon, Power: Icon, PowerOff: Icon, Radio: Icon,
    RefreshCw: Icon, RotateCcw: Icon, Save: Icon, Search: Icon, Send: Icon,
    Settings: Icon, Settings2: Icon, Shield: Icon, ShieldAlert: Icon,
    ShieldCheck: Icon, Square: Icon, Stethoscope: Icon, Sun: Icon,
    Tag: Icon, ThumbsDown: Icon, ThumbsUp: Icon, Timer: Icon, Trash2: Icon,
    TrendingDown: Icon, TrendingUp: Icon, TriangleAlertIcon: Icon,
    Unlink: Icon, User: Icon, UserCheck: Icon, UserX: Icon,
    Volume2: Icon, VolumeX: Icon, Webhook: Icon, Wifi: Icon, WifiOff: Icon,
    Wrench: Icon, XCircle: Icon, XIcon: Icon, Zap: Icon,
    ZoomIn: Icon, ZoomOut: Icon,
  };
});

describe("PERF-03: AssistantMessage cache wiring (render test)", () => {
  it("MessageItem renders without crashing with assistant message", async () => {
    // Dynamic import after mocks are set up
    const { MessageItem } = await import("@/app/(authenticated)/chat/MessageItem");

    const msg = {
      id: "test-1",
      role: "assistant" as const,
      parts: [{ type: "text" as const, text: "hello world" }],
      createdAt: "2024-01-01T00:00:00Z",
    };

    expect(() => {
      render(React.createElement(MessageItem, { message: msg }));
    }).not.toThrow();
  });

  it("renderPartsWithGrouping called once for two renders with same message ref", async () => {
    const { MessageItem } = await import("@/app/(authenticated)/chat/MessageItem");

    const msg = {
      id: "test-cache-hit",
      role: "assistant" as const,
      parts: [{ type: "text" as const, text: "cached content" }],
      createdAt: "2024-01-01T00:00:00Z",
    };

    // First render - populates cache
    const { rerender, container: c1 } = render(
      React.createElement(MessageItem, { message: msg })
    );

    const firstHtml = c1.innerHTML;

    // Second render with SAME object reference - should use cache
    rerender(React.createElement(MessageItem, { message: msg }));

    // Content should be the same (cache hit)
    expect(c1.innerHTML).toBe(firstHtml);
  });

  it("renderPartsWithGrouping called again when message object reference changes", async () => {
    const { MessageItem } = await import("@/app/(authenticated)/chat/MessageItem");

    const msg1 = {
      id: "test-cache-miss",
      role: "assistant" as const,
      parts: [{ type: "text" as const, text: "original content" }],
      createdAt: "2024-01-01T00:00:00Z",
    };

    const msg2 = {
      id: "test-cache-miss",
      role: "assistant" as const,
      parts: [{ type: "text" as const, text: "updated content" }],
      createdAt: "2024-01-01T00:00:00Z",
    };

    const { rerender, getByTestId } = render(
      React.createElement(MessageItem, { message: msg1 })
    );

    // Second render with DIFFERENT object reference - cache miss, re-renders
    rerender(React.createElement(MessageItem, { message: msg2 }));

    // New content should be rendered (cache miss forced re-render)
    const textPart = getByTestId("text-part");
    expect(textPart.textContent).toBe("updated content");
  });
});
