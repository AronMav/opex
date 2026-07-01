import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

// Mock Radix-based Tabs so fireEvent.click activates panels in jsdom
// (Radix Tabs relies on pointer events that don't fire with fireEvent.click).
vi.mock("@/components/ui/tabs", () => {
  function Tabs({ children, defaultValue }: { children: React.ReactNode; defaultValue?: string }) {
    const [active, setActive] = React.useState(defaultValue ?? "");
    return (
      <div data-testid="tabs">
        {React.Children.map(children, (child) => {
          if (!React.isValidElement(child)) return child;
          return React.cloneElement(child as React.ReactElement<{ activeTab?: string; onTabChange?: (v: string) => void }>, {
            activeTab: active,
            onTabChange: setActive,
          });
        })}
      </div>
    );
  }
  function TabsList({ children, activeTab, onTabChange }: { children: React.ReactNode; activeTab?: string; onTabChange?: (v: string) => void }) {
    return (
      <div role="tablist">
        {React.Children.map(children, (child) => {
          if (!React.isValidElement(child)) return child;
          return React.cloneElement(child as React.ReactElement<{ activeTab?: string; onTabChange?: (v: string) => void }>, {
            activeTab,
            onTabChange,
          });
        })}
      </div>
    );
  }
  function TabsTrigger({ children, value, activeTab, onTabChange }: { children: React.ReactNode; value: string; activeTab?: string; onTabChange?: (v: string) => void }) {
    return (
      <button role="tab" aria-selected={activeTab === value} onClick={() => onTabChange?.(value)}>
        {children}
      </button>
    );
  }
  function TabsContent({ children, value, activeTab }: { children: React.ReactNode; value: string; activeTab?: string }) {
    if (activeTab !== value) return null;
    return <div role="tabpanel">{children}</div>;
  }
  return { Tabs, TabsList, TabsTrigger, TabsContent };
});

const mutate = vi.fn();
// ToolsPage calls useQueryClient() directly (page.tsx) — without this mock the
// hook throws "No QueryClient set" (there is no test-wide QueryClientProvider).
vi.mock("@tanstack/react-query", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-query")>();
  return {
    ...actual,
    useQueryClient: () => ({ invalidateQueries: vi.fn(), setQueryData: vi.fn() }),
  };
});
vi.mock("@/lib/queries", () => ({
  qk: { handlers: ["handlers"], handlerAllowlist: ["handlers", "allowlist"] },
  useYamlTools: () => ({ data: [], isLoading: false, error: null }),
  useMcpServers: () => ({ data: [], isLoading: false, error: null }),
  useHandlers: () => ({
    data: [
      { id: "transcribe", labels: { en: "Transcribe" }, descriptions: { en: "STT" },
        icon: "mic", match: { mime: ["audio/*"] }, capability: "stt", provider: "Whisper",
        execution: "async", output: "text", order: 10, tier: "builtin", enabled: true },
      { id: "my_handler", labels: { en: "My Handler" }, descriptions: {},
        icon: "", match: {}, execution: "sync", output: "text", order: 20,
        tier: "workspace", enabled: true },
    ],
    isLoading: false, error: null,
  }),
  useHandlerAllowlist: () => ({ data: [] }),
  useSetHandlerAllowlist: () => ({ mutate, isPending: false }),
}));

import ToolsPage from "../page";

describe("File Handlers tab", () => {
  beforeEach(() => mutate.mockClear());

  it("renders a card per handler and toggles a builtin via PUT", async () => {
    render(<ToolsPage />);
    // Switch to the handlers tab.
    fireEvent.click(screen.getByRole("tab", { name: /File Handlers|Обработчики/i }));
    expect(await screen.findByText("Transcribe")).toBeInTheDocument();
    expect(screen.getByText("My Handler")).toBeInTheDocument();
    // The builtin has a Switch; toggling it fires the mutation with its id.
    const toggle = screen.getByLabelText("transcribe");
    fireEvent.click(toggle);
    // mutate is called with the toggle args + an options object carrying the
    // per-row onSettled (clears the in-flight row's pending state).
    expect(mutate).toHaveBeenCalledWith(
      { action_ref: "transcribe", enabled: false },
      expect.objectContaining({ onSettled: expect.any(Function) }),
    );
  });
});
