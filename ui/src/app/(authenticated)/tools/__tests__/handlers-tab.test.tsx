import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";

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
const deleteMutate = vi.fn();
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
        execution: "async", output: "text", order: 10, tier: "builtin", enabled: true,
        source: "builtin" },
      { id: "my_handler", labels: { en: "My Handler" }, descriptions: {},
        icon: "", match: {}, execution: "sync", output: "text", order: 20,
        tier: "workspace", enabled: true, source: "workspace" },
    ],
    isLoading: false, error: null,
  }),
  useHandlerAllowlist: () => ({ data: [] }),
  useSetHandlerAllowlist: () => ({ mutate, isPending: false }),
  useHandlerSource: () => ({ data: null, isLoading: false }),
  useCreateHandler: () => ({ mutate: vi.fn(), isPending: false }),
  useUpdateHandler: () => ({ mutate: vi.fn(), isPending: false }),
  useDeleteHandler: () => ({ mutate: deleteMutate, isPending: false, variables: null }),
}));

import ToolsPage from "../page";

describe("File Handlers tab", () => {
  beforeEach(() => { mutate.mockClear(); deleteMutate.mockClear(); });

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

  it("workspace card shows Edit and Delete; builtin card shows Edit but not Delete", async () => {
    render(<ToolsPage />);
    fireEvent.click(screen.getByRole("tab", { name: /File Handlers|Обработчики/i }));
    expect(await screen.findByText("My Handler")).toBeInTheDocument();

    // Both cards should have an Edit button (aria-label).
    const editButtons = screen.getAllByRole("button", { name: /Edit|Редактировать/i });
    expect(editButtons.length).toBeGreaterThanOrEqual(2);

    // The workspace card should have a Delete button.
    const deleteButtons = screen.getAllByRole("button", { name: /Delete|Удалить/i });
    expect(deleteButtons.length).toBeGreaterThanOrEqual(1);

    // The builtin card (transcribe, source="builtin") must NOT have a Delete/Reset button.
    // Only the workspace card delete button should exist.
    expect(deleteButtons.length).toBe(1);
  });

  it("clicking Delete on the workspace card opens a confirm dialog without mutating yet", async () => {
    render(<ToolsPage />);
    fireEvent.click(screen.getByRole("tab", { name: /File Handlers|Обработчики/i }));
    expect(await screen.findByText("My Handler")).toBeInTheDocument();

    const deleteButton = screen.getByRole("button", { name: /Delete|Удалить/i });
    fireEvent.click(deleteButton);
    // Destructive action must be gated behind a confirm dialog — no immediate mutate.
    expect(deleteMutate).not.toHaveBeenCalled();
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
  });

  it("confirming the dialog calls deleteHandler.mutate with the handler id", async () => {
    render(<ToolsPage />);
    fireEvent.click(screen.getByRole("tab", { name: /File Handlers|Обработчики/i }));
    expect(await screen.findByText("My Handler")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Delete|Удалить/i }));
    const dialog = await screen.findByRole("alertdialog");
    // The dialog's own destructive action button (not the row's Delete trigger).
    const confirmButton = within(dialog).getByRole("button", { name: /Delete|Удалить/i });
    fireEvent.click(confirmButton);
    expect(deleteMutate).toHaveBeenCalledWith("my_handler");
  });
});
