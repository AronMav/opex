import { vi, describe, it, expect } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// H6: every authenticated page must carry a skip-to-content link pointing at the
// <main id="main-content"> landmark.

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  usePathname: () => "/chat",
}));

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { token: "test-token", isAuthenticated: true, restore: vi.fn().mockResolvedValue(true) };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token" }) },
  ),
}));

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn().mockResolvedValue({ needs_setup: false }),
  apiPost: vi.fn().mockResolvedValue({}),
}));

vi.mock("@/lib/query-client", () => ({ queryClient: { invalidateQueries: vi.fn(), setQueryData: vi.fn() } }));
vi.mock("@/lib/queries", () => ({ qk: { sessions: (a: string) => ["sessions", a] } }));
vi.mock("@/lib/nav", () => ({ pageHasOwnHeader: () => true }));

vi.mock("@/stores/ws-store", () => ({
  useWsStore: (selector?: (s: Record<string, unknown>) => unknown) => {
    const state = { connected: true, connect: vi.fn(), disconnect: vi.fn() };
    return selector ? selector(state) : state;
  },
}));

vi.mock("@/stores/chat-store", () => ({
  useChatStore: { getState: () => ({ setThinking: vi.fn() }) },
}));

vi.mock("@/hooks/use-ws-subscription", () => ({ useWsSubscription: vi.fn() }));
vi.mock("@/hooks/use-translation", () => ({ useTranslation: () => ({ t: (k: string) => k, locale: "en" }) }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

vi.mock("@/components/ui/sidebar", () => ({
  SidebarProvider: ({ children }: { children: React.ReactNode }) => children,
  SidebarInset: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  SidebarTrigger: () => null,
}));
vi.mock("@/components/app-sidebar", () => ({ AppSidebar: () => null }));
vi.mock("@/providers/query-provider", () => ({ QueryProvider: ({ children }: { children: React.ReactNode }) => children }));

import AuthenticatedLayout from "../layout";

describe("Authenticated layout a11y (H6)", () => {
  it("renders a skip link targeting the main-content landmark", async () => {
    render(<AuthenticatedLayout><div>child</div></AuthenticatedLayout>);
    await waitFor(() => expect(screen.getByText("child")).toBeInTheDocument());

    const link = screen.getByRole("link", { name: /skip/i });
    expect(link).toHaveAttribute("href", "#main-content");

    const main = document.getElementById("main-content");
    expect(main?.tagName).toBe("MAIN");
  });
});
