import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Pure function imports ───────────────────────────────────────────────────
import { detailToForm, formToPayload, emptyForm } from "../page";
import type { FormState, AgentEditDialogProps } from "../AgentEditDialog";

// ── Mocks for UI tests ─────────────────────────────────────────────────────

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
        agents: ["main"],
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

vi.mock("./RoutingRulesEditor", () => ({
  PROVIDERS: [],
  FALLBACK_MODELS: {},
  RoutingRulesEditor: () => null,
}));

// ── Helpers ─────────────────────────────────────────────────────────────────

/** Build a minimal AgentDetail-shaped object. Fills required fields with
 *  sensible defaults so individual tests only override what they care about. */
function makeDetail(overrides: Record<string, unknown> = {}) {
  return {
    name: "TestAgent",
    language: "ru",
    provider: "openai",
    model: "gpt-4",
    provider_connection: null,
    fallback_provider: null,
    temperature: 1.0,
    max_tokens: null,
    access: null,
    heartbeat: null,
    tools: null,
    compaction: null,
    session: null,
    icon: null,
    max_tools_in_context: null,
    tool_loop: null,
    approval: null,
    routing: [],
    watchdog: null,
    hooks: null,
    max_history_messages: null,
    daily_budget_tokens: 0,
    max_agent_turns: null,
    max_failover_attempts: 0,
    is_running: false,
    config_dirty: false,
    voice: undefined,
    ...overrides,
  };
}

// ── formToPayload tests ─────────────────────────────────────────────────────

describe("formToPayload", () => {
  const base: FormState = { ...emptyForm };

  it("empty maxTokens → max_tokens: null", () => {
    const payload = formToPayload({ ...base, maxTokens: "" });
    expect(payload.max_tokens).toBeNull();
  });

  it('maxTokens: "512" → max_tokens: 512', () => {
    const payload = formToPayload({ ...base, maxTokens: "512" });
    expect(payload.max_tokens).toBe(512);
  });

  it('empty providerConnection → provider_connection: null', () => {
    const payload = formToPayload({ ...base, providerConnection: "" });
    expect(payload.provider_connection).toBeNull();
  });

  it('providerConnection: "openai" → provider_connection: "openai"', () => {
    const payload = formToPayload({ ...base, providerConnection: "openai" });
    expect(payload.provider_connection).toBe("openai");
  });

  it('fallbackProvider: "" → fallback_provider: "" (empty string, not null)', () => {
    const payload = formToPayload({ ...base, fallbackProvider: "" });
    expect(payload.fallback_provider).toBe("");
  });

  it('fallbackProvider: "anthropic" → fallback_provider: "anthropic"', () => {
    const payload = formToPayload({ ...base, fallbackProvider: "anthropic" });
    expect(payload.fallback_provider).toBe("anthropic");
  });

  it("toolsEnabled: false → tools: null", () => {
    const payload = formToPayload({ ...base, toolsEnabled: false });
    expect(payload.tools).toBeNull();
  });

  it('toolsEnabled: true, toolsDeny: "code_exec, workspace_delete" → tools.deny: ["code_exec", "workspace_delete"]', () => {
    const payload = formToPayload({
      ...base,
      toolsEnabled: true,
      toolsDeny: "code_exec, workspace_delete",
    });
    expect(payload.tools).not.toBeNull();
    expect(payload.tools!.deny).toEqual(["code_exec", "workspace_delete"]);
  });

  it("toolsEnabled: true, toolGroupGit: false → tools.groups.git: false", () => {
    const payload = formToPayload({
      ...base,
      toolsEnabled: true,
      toolGroupGit: false,
    });
    expect(payload.tools).not.toBeNull();
    expect(payload.tools!.groups.git).toBe(false);
  });

  it("accessEnabled: false → access: null", () => {
    const payload = formToPayload({ ...base, accessEnabled: false });
    expect(payload.access).toBeNull();
  });

  it('accessEnabled: true, accessMode: "restricted" → access.mode: "restricted"', () => {
    const payload = formToPayload({
      ...base,
      accessEnabled: true,
      accessMode: "restricted",
    });
    expect(payload.access).not.toBeNull();
    expect(payload.access!.mode).toBe("restricted");
  });

  it('temperature: "0.7" → temperature: 0.7', () => {
    const payload = formToPayload({ ...base, temperature: "0.7" });
    expect(payload.temperature).toBe(0.7);
  });

  it('dailyBudgetTokens: "0" → daily_budget_tokens: 0', () => {
    const payload = formToPayload({ ...base, dailyBudgetTokens: "0" });
    expect(payload.daily_budget_tokens).toBe(0);
  });

  it('temperature: "NaN" → falls back to 1.0', () => {
    const payload = formToPayload({ ...base, temperature: "NaN" });
    expect(payload.temperature).toBe(1.0);
  });

  it('temperature: "abc" → falls back to 1.0', () => {
    const payload = formToPayload({ ...base, temperature: "abc" });
    expect(payload.temperature).toBe(1.0);
  });

  it("compEnabled: true maps compaction fields correctly", () => {
    const payload = formToPayload({
      ...base,
      compEnabled: true,
      compThreshold: "0.75",
      compPreserveLastN: "5",
    });
    expect(payload.compaction).not.toBeNull();
    expect(payload.compaction!.threshold).toBe(0.75);
    expect(payload.compaction!.preserve_last_n).toBe(5);
  });

  it("compEnabled: false → compaction: null", () => {
    const payload = formToPayload({ ...base, compEnabled: false });
    expect(payload.compaction).toBeNull();
  });

  it("tlEnabled: false → tool_loop: null", () => {
    const payload = formToPayload({ ...base, tlEnabled: false });
    expect(payload.tool_loop).toBeNull();
  });

  it("sessionEnabled: false → session: null", () => {
    const payload = formToPayload({ ...base, sessionEnabled: false });
    expect(payload.session).toBeNull();
  });

  it("approvalEnabled: false → approval: null", () => {
    const payload = formToPayload({ ...base, approvalEnabled: false });
    expect(payload.approval).toBeNull();
  });

  it("hbEnabled: false → heartbeat: null", () => {
    const payload = formToPayload({ ...base, hbEnabled: false });
    expect(payload.heartbeat).toBeNull();
  });
});

// ── detailToForm tests ──────────────────────────────────────────────────────

describe("detailToForm", () => {
  it("provider_connection: null → providerConnection: ''", () => {
    const form = detailToForm(makeDetail({ provider_connection: null }) as any);
    expect(form.providerConnection).toBe("");
  });

  it('provider_connection: "openai" → providerConnection: "openai"', () => {
    const form = detailToForm(makeDetail({ provider_connection: "openai" }) as any);
    expect(form.providerConnection).toBe("openai");
  });

  it("max_tokens: null → maxTokens: ''", () => {
    const form = detailToForm(makeDetail({ max_tokens: null }) as any);
    expect(form.maxTokens).toBe("");
  });

  it("max_tokens: 1024 → maxTokens: '1024'", () => {
    const form = detailToForm(makeDetail({ max_tokens: 1024 }) as any);
    expect(form.maxTokens).toBe("1024");
  });

  it("fallback_provider: undefined → fallbackProvider: ''", () => {
    const form = detailToForm(makeDetail({ fallback_provider: undefined }) as any);
    expect(form.fallbackProvider).toBe("");
  });

  it('maps fallback_provider empty string to fallbackProvider empty string', () => {
    const form = detailToForm(makeDetail({ fallback_provider: "" }) as any);
    expect(form.fallbackProvider).toBe("");
  });

  it('fallback_provider: "anthropic" → fallbackProvider: "anthropic"', () => {
    const form = detailToForm(makeDetail({ fallback_provider: "anthropic" }) as any);
    expect(form.fallbackProvider).toBe("anthropic");
  });

  it("tools: null → toolsEnabled: false", () => {
    const form = detailToForm(makeDetail({ tools: null }) as any);
    expect(form.toolsEnabled).toBe(false);
  });

  it('tools with deny: ["code_exec"] → toolsDeny: "code_exec"', () => {
    const form = detailToForm(
      makeDetail({
        tools: {
          allow: [],
          deny: ["code_exec"],
          allow_all: true,
          deny_all_others: false,
          groups: { git: true, tool_management: true, skill_editing: true, session_tools: true },
        },
      }) as any
    );
    expect(form.toolsDeny).toBe("code_exec");
  });

  it("tools.groups.git: false → toolGroupGit: false", () => {
    const form = detailToForm(
      makeDetail({
        tools: {
          allow: [],
          deny: [],
          allow_all: true,
          deny_all_others: false,
          groups: { git: false, tool_management: true, skill_editing: true, session_tools: true },
        },
      }) as any
    );
    expect(form.toolGroupGit).toBe(false);
  });

  it("access: null → accessEnabled: false", () => {
    const form = detailToForm(makeDetail({ access: null }) as any);
    expect(form.accessEnabled).toBe(false);
  });

  it('access: { mode: "restricted" } → accessEnabled: true, accessMode: "restricted"', () => {
    const form = detailToForm(
      makeDetail({ access: { mode: "restricted", owner_id: null } }) as any
    );
    expect(form.accessEnabled).toBe(true);
    expect(form.accessMode).toBe("restricted");
  });
});

// ── AgentEditDialog UI tests ─────────────────────────────────────────────────

describe("AgentEditDialog UI", () => {
  let AgentEditDialog: React.ComponentType<AgentEditDialogProps>;

  beforeEach(async () => {
    const mod = await import("../AgentEditDialog");
    AgentEditDialog = mod.AgentEditDialog;
  });

  function makeProps(overrides: Partial<AgentEditDialogProps> = {}): AgentEditDialogProps {
    return {
      open: true,
      onOpenChange: vi.fn(),
      editName: null,
      form: { ...emptyForm },
      upd: vi.fn(),
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
      ...overrides,
    };
  }

  it("typing in the name field calls upd with the new name", () => {
    const upd = vi.fn();
    const props = makeProps({ upd });
    render(<AgentEditDialog {...props} />);

    const nameInput = screen.getAllByRole("textbox")[0];
    fireEvent.change(nameInput, { target: { value: "my-new-agent" } });

    expect(upd).toHaveBeenCalledWith(expect.objectContaining({ name: "my-new-agent" }));
  });

  it("clicking Save calls onSave", () => {
    const onSave = vi.fn();
    const props = makeProps({ onSave, canSave: true });
    render(<AgentEditDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create/i });
    fireEvent.click(saveButton);

    expect(onSave).toHaveBeenCalled();
  });

  it("Save button is disabled when canSave is false", () => {
    const props = makeProps({ canSave: false });
    render(<AgentEditDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create/i });
    expect(saveButton).toBeDisabled();
  });

  it("invalid name with '@' shows error message", () => {
    const props = makeProps({
      form: { ...emptyForm, name: "invalid@name" },
    });
    render(<AgentEditDialog {...props} />);

    expect(
      screen.getByText(/only letters, numbers, hyphens and underscores allowed/i)
    ).toBeInTheDocument();
  });
});
