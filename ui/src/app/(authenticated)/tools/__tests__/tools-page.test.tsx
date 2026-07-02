import React from "react";
import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// Mock Radix-based Tabs so panels render deterministically in jsdom.
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

// ToolsPage calls useQueryClient() directly — provide a stub so it doesn't throw.
vi.mock("@tanstack/react-query", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-query")>();
  return {
    ...actual,
    useQueryClient: () => ({ invalidateQueries: vi.fn(), setQueryData: vi.fn() }),
  };
});

vi.mock("@/lib/queries", () => ({
  qk: { yamlTools: ["yaml"], mcpServers: ["mcp"], handlers: ["handlers"] },
  useYamlTools: () => ({
    data: [
      {
        name: "searxng-search",
        description: "Search the web",
        method: "get",
        endpoint: "http://searxng:8080/search",
        status: "verified",
      },
    ],
    isLoading: false,
    error: null,
  }),
  useMcpServers: () => ({ data: [], isLoading: false, error: null }),
  useHandlers: () => ({ data: [], isLoading: false, error: null }),
  useSetHandlerAllowlist: () => ({ mutate: vi.fn(), isPending: false }),
  useHandlerSource: () => ({ data: null, isLoading: false }),
  useDeleteHandler: () => ({ mutate: vi.fn(), isPending: false, variables: null }),
}));

import ToolsPage from "../page";

describe("ToolsPage (design-system migration)", () => {
  it("renders the page header, the three tabs, and a yaml card TypeBadge", () => {
    render(<ToolsPage />);

    // PageHeader title (tools.title translation key or its resolved label).
    expect(
      screen.getByRole("heading", { name: /tools\.title|Tools|Инструменты/i }),
    ).toBeInTheDocument();

    // Three tabs render.
    const tabs = screen.getAllByRole("tab");
    expect(tabs.length).toBe(3);

    // The default (external/yaml) tab shows a card with a GET TypeBadge.
    expect(screen.getByText("GET")).toBeInTheDocument();
    // …and the yaml tool name.
    expect(screen.getByText("searxng-search")).toBeInTheDocument();
  });
});
