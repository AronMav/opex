import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import type { AgentInfo } from "@/types/api";

// ── Mocks ───────────────────────────────────────────────────────────────────

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
  usePathname: () => "/",
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

vi.mock("@/stores/auth-store", () => ({
  useAuthStore: Object.assign(
    (selector?: (s: Record<string, unknown>) => unknown) => {
      const state = { token: "test-token", isAuthenticated: true, agents: ["main"] };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token", restore: vi.fn() }) }
  ),
}));

// The heavy child dialogs are irrelevant to the card-grid rendering assertions.
vi.mock("../AgentEditDialog", () => ({
  AgentEditDialog: () => null,
  ChannelDialog: () => null,
  DeleteChannelDialog: () => null,
}));

vi.mock("../RoutingRulesEditor", () => ({}));

// Only `apiGet("/api/agents")` matters for the initial render.
const { apiGet } = vi.hoisted(() => ({ apiGet: vi.fn() }));
vi.mock("@/lib/api", () => ({
  apiGet,
  apiPost: vi.fn(),
  apiPut: vi.fn(),
  apiDelete: vi.fn(),
}));

// ── Fixtures ─────────────────────────────────────────────────────────────────

function makeAgent(overrides: Partial<AgentInfo> = {}): AgentInfo {
  return {
    name: "main",
    language: "en",
    profile: "Default",
    capabilities: { text: true, stt: false, tts: false, vision: false, imagegen: false, websearch: false },
    icon_url: null,
    temperature: 1,
    has_access: false,
    access_mode: null,
    has_heartbeat: false,
    heartbeat_cron: null,
    heartbeat_timezone: null,
    tool_policy: null,
    routing_count: 0,
    is_running: true,
    config_dirty: false,
    base: true,
    ...overrides,
  } as AgentInfo;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("AgentsPage — card grid", () => {
  it("renders a running agent card with a success StatusBadge", async () => {
    apiGet.mockResolvedValueOnce({ agents: [makeAgent({ name: "runner", is_running: true })] });
    const { default: AgentsPage } = await import("../page");
    render(<AgentsPage />);

    expect(await screen.findByText("runner")).toBeInTheDocument();
    // StatusBadge maps "running" → Badge variant "success" (data-variant on the span).
    const badge = document.querySelector('[data-slot="badge"]');
    expect(badge).toHaveAttribute("data-variant", "success");
    expect(within(badge as HTMLElement).getByText("agents.active")).toBeInTheDocument();
  });

  it("renders an inactive agent card with a secondary StatusBadge", async () => {
    apiGet.mockResolvedValueOnce({ agents: [makeAgent({ name: "idle", is_running: false })] });
    const { default: AgentsPage } = await import("../page");
    render(<AgentsPage />);

    expect(await screen.findByText("idle")).toBeInTheDocument();
    const badge = document.querySelector('[data-slot="badge"]');
    // "inactive" is not in STATUS_VARIANT → falls back to "secondary".
    expect(badge).toHaveAttribute("data-variant", "secondary");
    expect(within(badge as HTMLElement).getByText("agents.inactive")).toBeInTheDocument();
  });

  it("renders both running and inactive badges when multiple agents are present", async () => {
    apiGet.mockResolvedValueOnce({
      agents: [
        makeAgent({ name: "alpha", is_running: true, base: false }),
        makeAgent({ name: "beta", is_running: false, base: false }),
      ],
    });
    const { default: AgentsPage } = await import("../page");
    render(<AgentsPage />);

    await waitFor(() => {
      expect(screen.getByText("alpha")).toBeInTheDocument();
      expect(screen.getByText("beta")).toBeInTheDocument();
    });
    const variants = Array.from(document.querySelectorAll('[data-slot="badge"]')).map((b) =>
      b.getAttribute("data-variant"),
    );
    expect(variants).toContain("success");
    expect(variants).toContain("secondary");
  });
});
