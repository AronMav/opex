import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Imports ─────────────────────────────────────────────────────────────────
import { emptyForm } from "../page";
import { AgentEditDialog } from "../AgentEditDialog";
import type { FormState, AgentEditDialogProps } from "../AgentEditDialog";

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
      const state = {
        token: "test-token",
        isAuthenticated: true,
        agents: [],
        agentIcons: {},
        lastFetched: Date.now(),
        login: vi.fn(),
        logout: vi.fn(),
        restore: vi.fn(),
        refreshIfStale: vi.fn(),
      };
      return selector ? selector(state) : state;
    },
    { getState: () => ({ token: "test-token" }) }
  ),
}));

vi.mock("@/lib/queries", () => ({
  useProviders: () => ({ data: [] }),
  useProviderModels: () => ({ data: [], isLoading: false, refetch: vi.fn() }),
}));

vi.mock("@/components/ui/cron-schedule-picker", () => ({
  CronSchedulePicker: () => null,
}));

vi.mock("./RoutingRulesEditor", () => ({
  PROVIDERS: [],
  FALLBACK_MODELS: {},
  RoutingRulesEditor: () => null,
}));

// ── Helper ───────────────────────────────────────────────────────────────────

function makeProps(formOverride: Partial<FormState> = {}, updFn = vi.fn()): AgentEditDialogProps {
  return {
    open: true,
    onOpenChange: vi.fn(),
    editName: "test-agent",
    form: { ...emptyForm, ...formOverride },
    upd: updFn,
    saving: false,
    canSave: true,
    onSave: vi.fn(),
    discoveredModels: {},
    fetchModels: vi.fn(),
    toolNames: [],
    secretNames: [],
    voices: [],
    channels: [],
    channelSaving: false,
    onOpenChannelDialog: vi.fn(),
    onRestartChannel: vi.fn(),
    onDeleteChannelRequest: vi.fn(),
  };
}

// ── Tools tab ────────────────────────────────────────────────────────────────

describe("AgentEditDialog — Tools tab", () => {
  it("toolsEnabled switch click → upd({toolsEnabled: true})", () => {
    const upd = vi.fn();
    // Render with all relevant switches checked=true except toolsEnabled=false
    // This makes the unchecked switch identifiable
    render(
      <AgentEditDialog
        {...makeProps(
          {
            toolsEnabled: false,
            approvalEnabled: true,
            tlEnabled: true,
            sessionEnabled: true,
            accessEnabled: true,
            hbEnabled: true,
            hooksLogAll: true,
          },
          upd
        )}
      />
    );
    const switches = screen.getAllByRole("switch");
    const unchecked = switches.find((s) => s.getAttribute("aria-checked") === "false");
    expect(unchecked).toBeTruthy();
    fireEvent.click(unchecked!);
    expect(upd).toHaveBeenCalledWith({ toolsEnabled: true });
  });

  it("toolGroupGit checkbox click → upd({toolGroupGit: true})", () => {
    const upd = vi.fn();
    // Render with toolGroupGit=false so clicking it calls upd({toolGroupGit: true})
    render(
      <AgentEditDialog
        {...makeProps(
          {
            toolsEnabled: true,
            toolGroupGit: false,
            toolGroupManagement: true,
            toolGroupSkillEditing: true,
            toolGroupSessionTools: true,
          },
          upd
        )}
      />
    );
    // When toolsEnabled=true, the 4 tool group checkboxes are rendered.
    // toolGroupGit is first (index 0) in the grid.
    const checkboxes = screen.getAllByRole("checkbox");
    const gitCheckbox = checkboxes[0];
    expect((gitCheckbox as HTMLInputElement).checked).toBe(false);
    fireEvent.click(gitCheckbox);
    expect(upd).toHaveBeenCalledWith({ toolGroupGit: true });
  });

  it("approvalEnabled switch click → upd({approvalEnabled: true})", () => {
    const upd = vi.fn();
    // Render with toolsEnabled=true (so toolsEnabled switch is checked)
    // approvalEnabled=false → its switch will be the unchecked one
    render(
      <AgentEditDialog
        {...makeProps(
          {
            toolsEnabled: true,
            toolsAllowAll: true,
            approvalEnabled: false,
            tlEnabled: true,
            sessionEnabled: true,
            accessEnabled: true,
            hbEnabled: true,
            hooksLogAll: true,
          },
          upd
        )}
      />
    );
    const switches = screen.getAllByRole("switch");
    const unchecked = switches.find((s) => s.getAttribute("aria-checked") === "false");
    expect(unchecked).toBeTruthy();
    fireEvent.click(unchecked!);
    expect(upd).toHaveBeenCalledWith({ approvalEnabled: true });
  });

  it('approvalCategories "system" checkbox click → upd({approvalCategories: ["system"]})', () => {
    const upd = vi.fn();
    // Render with approvalEnabled=true but toolsEnabled=false (default).
    // When toolsEnabled=false, tool group checkboxes are hidden (SwitchSection disabled).
    // So only the 3 approval category checkboxes exist in DOM: system[0], destructive[1], external[2].
    render(
      <AgentEditDialog
        {...makeProps(
          {
            approvalEnabled: true,
            approvalCategories: [],
          },
          upd
        )}
      />
    );
    const checkboxes = screen.getAllByRole("checkbox");
    // system is index 0, unchecked (approvalCategories: [])
    const systemCheckbox = checkboxes[0];
    expect((systemCheckbox as HTMLInputElement).checked).toBe(false);
    fireEvent.click(systemCheckbox);
    expect(upd).toHaveBeenCalledWith({ approvalCategories: ["system"] });
  });

  it("approvalTimeout input change → upd({approvalTimeout: '600'})", () => {
    const upd = vi.fn();
    render(
      <AgentEditDialog
        {...makeProps(
          {
            approvalEnabled: true,
            approvalCategories: [],
            approvalTimeout: "300",
          },
          upd
        )}
      />
    );
    // approvalTimeout is a number input; find by value "300"
    const input = screen.getByDisplayValue("300");
    fireEvent.change(input, { target: { value: "600" } });
    expect(upd).toHaveBeenCalledWith({ approvalTimeout: "600" });
  });
});

// ── Behavior tab ─────────────────────────────────────────────────────────────

describe("AgentEditDialog — Behavior tab", () => {
  it("tlEnabled switch click → upd({tlEnabled: true})", () => {
    const upd = vi.fn();
    // All other section switches true, tlEnabled=false
    render(
      <AgentEditDialog
        {...makeProps(
          {
            tlEnabled: false,
            toolsEnabled: true,
            approvalEnabled: true,
            sessionEnabled: true,
            accessEnabled: true,
            hbEnabled: true,
            hooksLogAll: true,
          },
          upd
        )}
      />
    );
    const switches = screen.getAllByRole("switch");
    const unchecked = switches.find((s) => s.getAttribute("aria-checked") === "false");
    expect(unchecked).toBeTruthy();
    fireEvent.click(unchecked!);
    expect(upd).toHaveBeenCalledWith({ tlEnabled: true });
  });

  it("tlMaxIterations input change → upd({tlMaxIterations: '100'})", () => {
    const upd = vi.fn();
    render(
      <AgentEditDialog
        {...makeProps(
          {
            tlEnabled: true,
            tlMaxIterations: "50",
          },
          upd
        )}
      />
    );
    const input = screen.getByDisplayValue("50");
    fireEvent.change(input, { target: { value: "100" } });
    expect(upd).toHaveBeenCalledWith({ tlMaxIterations: "100" });
  });

  it("hooksBlockTools input change → upd({hooksBlockTools: 'tool1'})", () => {
    const upd = vi.fn();
    render(
      <AgentEditDialog
        {...makeProps(
          {
            hooksLogAll: true,
            hooksBlockTools: "",
          },
          upd
        )}
      />
    );
    // hooksBlockTools is an Input with placeholder "tool1, tool2"
    const input = screen.getByPlaceholderText("tool1, tool2");
    fireEvent.change(input, { target: { value: "tool1" } });
    expect(upd).toHaveBeenCalledWith({ hooksBlockTools: "tool1" });
  });
});

// ── Session tab ───────────────────────────────────────────────────────────────

describe("AgentEditDialog — Session tab", () => {
  it("sessionEnabled switch click → upd({sessionEnabled: true})", () => {
    const upd = vi.fn();
    // All other switches true, sessionEnabled=false
    render(
      <AgentEditDialog
        {...makeProps(
          {
            sessionEnabled: false,
            toolsEnabled: true,
            approvalEnabled: true,
            tlEnabled: true,
            accessEnabled: true,
            hbEnabled: true,
            hooksLogAll: true,
          },
          upd
        )}
      />
    );
    const switches = screen.getAllByRole("switch");
    const unchecked = switches.find((s) => s.getAttribute("aria-checked") === "false");
    expect(unchecked).toBeTruthy();
    fireEvent.click(unchecked!);
    expect(upd).toHaveBeenCalledWith({ sessionEnabled: true });
  });

  it("sessionTtlDays input change → upd({sessionTtlDays: '7'})", () => {
    const upd = vi.fn();
    render(
      <AgentEditDialog
        {...makeProps(
          {
            sessionEnabled: true,
            sessionTtlDays: "30",
          },
          upd
        )}
      />
    );
    const input = screen.getByDisplayValue("30");
    fireEvent.change(input, { target: { value: "7" } });
    expect(upd).toHaveBeenCalledWith({ sessionTtlDays: "7" });
  });

  it("accessEnabled switch click → upd({accessEnabled: true})", () => {
    const upd = vi.fn();
    // All other switches true, accessEnabled=false
    render(
      <AgentEditDialog
        {...makeProps(
          {
            accessEnabled: false,
            toolsEnabled: true,
            approvalEnabled: true,
            tlEnabled: true,
            sessionEnabled: true,
            hbEnabled: true,
            hooksLogAll: true,
          },
          upd
        )}
      />
    );
    const switches = screen.getAllByRole("switch");
    const unchecked = switches.find((s) => s.getAttribute("aria-checked") === "false");
    expect(unchecked).toBeTruthy();
    fireEvent.click(unchecked!);
    expect(upd).toHaveBeenCalledWith({ accessEnabled: true });
  });
});

// ── Schedule tab ──────────────────────────────────────────────────────────────

describe("AgentEditDialog — Schedule tab", () => {
  it("hbEnabled switch click → upd({hbEnabled: true})", () => {
    const upd = vi.fn();
    // All other switches true, hbEnabled=false
    render(
      <AgentEditDialog
        {...makeProps(
          {
            hbEnabled: false,
            toolsEnabled: true,
            approvalEnabled: true,
            tlEnabled: true,
            sessionEnabled: true,
            accessEnabled: true,
            hooksLogAll: true,
          },
          upd
        )}
      />
    );
    const switches = screen.getAllByRole("switch");
    const unchecked = switches.find((s) => s.getAttribute("aria-checked") === "false");
    expect(unchecked).toBeTruthy();
    fireEvent.click(unchecked!);
    expect(upd).toHaveBeenCalledWith({ hbEnabled: true });
  });
});
