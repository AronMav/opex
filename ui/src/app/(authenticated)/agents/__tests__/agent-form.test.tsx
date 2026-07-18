import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

// ── Pure function imports ───────────────────────────────────────────────────
import { detailToForm, formToPayload, emptyForm } from "../page";
import { soulGating } from "../AgentEditDialog";
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
  useProviderModelsDetailed: () => ({ data: [], isLoading: false, refetch: vi.fn() }),
}));

vi.mock("@/hooks/use-profiles", () => ({
  useProfiles: () => ({ data: { profiles: [{ id: "1", name: "Default", slots: {}, created_at: "", updated_at: "", agents: [] }] } }),
}));

vi.mock("./RoutingRulesEditor", () => ({
  RoutingRulesEditor: () => null,
}));

// AgentPromptsEditor (Prompts tab) reads the workspace prompt file via
// react-query — stub the prompt-library module so no QueryClient is needed.
vi.mock("@/lib/prompts", () => ({
  useAgentPrompts: () => ({ prompts: [], isLoading: false }),
  usePrompts: () => ({ prompts: [], isLoading: false }),
  agentPromptsKey: (n: string) => ["agent-prompts", n],
  agentPromptsPath: (n: string) => `agents/${n}/prompts.md`,
  serializePrompts: () => "",
  parsePrompts: () => [],
}));

// ── Helpers ─────────────────────────────────────────────────────────────────

/** Build a minimal AgentDetail-shaped object. Fills required fields with
 *  sensible defaults so individual tests only override what they care about. */
function makeDetail(overrides: Record<string, unknown> = {}) {
  return {
    name: "TestAgent",
    language: "ru",
    profile: "Default",
    capabilities: { text: true, stt: false, tts: false, vision: false, imagegen: false, websearch: false },
    temperature: 1.0,
    max_tokens: null,
    access: null,
    heartbeat: null,
    tools: null,
    compaction: null,
    session: null,
    max_tools_in_context: null,
    tool_loop: null,
    approval: null,
    routing: [],
    watchdog: null,
    hooks: null,
    max_history_messages: null,
    daily_budget_tokens: 0,
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

  it('profile: "Default" → payload.profile: "Default"', () => {
    const payload = formToPayload({ ...base, profile: "Default" });
    expect(payload.profile).toBe("Default");
  });

  it('profile: "Voice" → payload.profile: "Voice"', () => {
    const payload = formToPayload({ ...base, profile: "Voice" });
    expect(payload.profile).toBe("Voice");
  });

  it('profile: "" → payload.profile falls back to "Default"', () => {
    const payload = formToPayload({ ...base, profile: "" });
    expect(payload.profile).toBe("Default");
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

  it("round-trips the soul layer through detailToForm and formToPayload", () => {
    const detail: any = {
      name: "T", language: "ru", profile: "default", temperature: 1,
      capabilities: {}, routing: [], daily_budget_tokens: 0, max_failover_attempts: 3,
      is_running: false, config_dirty: false,
      soul: { enabled: true, reflection_threshold: 150, reflection_cooldown_minutes: 60,
              context_top_k: 6, context_budget_tokens: 800, max_events_per_session: 10 },
      drift: { enabled: true, threshold: 0.15, min_history: 6, baseline_turns: 3, correct: true, anchor: "You are T." },
      initiative: { enabled: true, daily_proposal_cap: 1, decompose: false, daily_plan: true,
                    auto_approve_day_plan: false, daily_token_budget: 0 },
      emotion: { enabled: true, intensity_importance_k: 3, blend_rate: 0.3, decay_half_life_hours: 12 },
    };
    const form = detailToForm(detail);
    expect(form.soulEnabled).toBe(true);
    expect(form.driftCorrect).toBe(true);
    expect(form.driftAnchor).toBe("You are T.");
    const payload: any = formToPayload(form);
    expect(payload.soul.enabled).toBe(true);
    expect(payload.drift.anchor).toBe("You are T.");
    expect(payload.emotion.enabled).toBe(true);
  });

  it("sends null-ish soul sections as disabled objects, not omitted", () => {
    const payload: any = formToPayload({ ...emptyForm });
    expect(payload.soul).toEqual(expect.objectContaining({ enabled: false }));
  });

  it("preserves an explicit 0 for drift.threshold and emotion.intensity_importance_k", () => {
    const f = { ...emptyForm, driftEnabled: true, driftThreshold: "0", emotionEnabled: true, soulEnabled: true, emotionK: "0" };
    const p: any = formToPayload(f);
    expect(p.drift.threshold).toBe(0);              // not 0.15
    expect(p.emotion.intensity_importance_k).toBe(0); // not 3
  });

  it("blank numeric field still falls back to the default", () => {
    const f = { ...emptyForm, driftEnabled: true, driftThreshold: "", emotionEnabled: true, soulEnabled: true, emotionK: "" };
    const p: any = formToPayload(f);
    expect(p.drift.threshold).toBe(0.15);
    expect(p.emotion.intensity_importance_k).toBe(3);
  });

  it("new-agent form default for driftBaselineTurns is 8 (matches backend v2 default)", () => {
    const p = formToPayload({ ...emptyForm });
    expect(p.drift.baseline_turns).toBe(8);
  });
});

// ── detailToForm tests ──────────────────────────────────────────────────────

describe("detailToForm", () => {
  it("profile: 'Default' → form.profile: 'Default'", () => {
    const form = detailToForm(makeDetail({ profile: "Default" }) as any);
    expect(form.profile).toBe("Default");
  });

  it('profile: "Voice" → form.profile: "Voice"', () => {
    const form = detailToForm(makeDetail({ profile: "Voice" }) as any);
    expect(form.profile).toBe("Voice");
  });

  it('profile: "" → form.profile falls back to "Default"', () => {
    const form = detailToForm(makeDetail({ profile: "" }) as any);
    expect(form.profile).toBe("Default");
  });

  it("max_tokens: null → maxTokens: ''", () => {
    const form = detailToForm(makeDetail({ max_tokens: null }) as any);
    expect(form.maxTokens).toBe("");
  });

  it("max_tokens: 1024 → maxTokens: '1024'", () => {
    const form = detailToForm(makeDetail({ max_tokens: 1024 }) as any);
    expect(form.maxTokens).toBe("1024");
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

  it("access: null → accessMode defaults to restricted (secure by default)", () => {
    const form = detailToForm(makeDetail({ access: null }) as any);
    expect(form.accessMode).toBe("restricted");
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
      toolNames: [],
      secretNames: [],
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
      screen.getByText(/agents\.name_invalid/i)
    ).toBeInTheDocument();
  });

  it("renders the profile select and a Manage profiles link", () => {
    const props = makeProps({ form: { ...emptyForm, profile: "Default" } });
    render(<AgentEditDialog {...props} />);

    // The profile <Select> combobox trigger is present (general tab, default view).
    expect(screen.getAllByRole("combobox").length).toBeGreaterThan(0);
    const manageLink = screen.getByText(/agents\.manage_profiles/i).closest("a");
    expect(manageLink).toHaveAttribute("href", "/profiles/");
  });
});

// ── soulGating tests ────────────────────────────────────────────────────────

describe("soulGating", () => {
  it("gates soul cross-fields", () => {
    expect(soulGating(
      { soulEnabled: false, driftEnabled: false, initiativeDailyPlan: false, initiativeTokenBudget: "0", hbEnabled: false },
      false,
    )).toEqual({
      emotionDisabled: true,
      driftCorrectDisabled: true,
      autoApproveDisabled: true,
      initiativeDisabled: false,
      dailyPlanDisabled: true,
    });
    expect(soulGating(
      { soulEnabled: true, driftEnabled: true, initiativeDailyPlan: true, initiativeTokenBudget: "5000", hbEnabled: true },
      true,
    )).toEqual({
      emotionDisabled: false,
      driftCorrectDisabled: false,
      autoApproveDisabled: false,
      initiativeDisabled: true,
      dailyPlanDisabled: true,
    });
  });

  it("dailyPlanDisabled requires a configured heartbeat even for non-base agents (M2)", () => {
    const g = soulGating(
      { soulEnabled: true, driftEnabled: true, initiativeDailyPlan: false, initiativeTokenBudget: "5000", hbEnabled: false },
      false,
    );
    expect(g.dailyPlanDisabled).toBe(true);

    const g2 = soulGating(
      { soulEnabled: true, driftEnabled: true, initiativeDailyPlan: false, initiativeTokenBudget: "5000", hbEnabled: true },
      false,
    );
    expect(g2.dailyPlanDisabled).toBe(false);
  });

  it("autoApproveDisabled requires a positive daily_token_budget even when daily_plan is on (M1)", () => {
    const zeroBudget = soulGating(
      { soulEnabled: true, driftEnabled: true, initiativeDailyPlan: true, initiativeTokenBudget: "0", hbEnabled: true },
      false,
    );
    expect(zeroBudget.autoApproveDisabled).toBe(true);

    const positiveBudget = soulGating(
      { soulEnabled: true, driftEnabled: true, initiativeDailyPlan: true, initiativeTokenBudget: "500", hbEnabled: true },
      false,
    );
    expect(positiveBudget.autoApproveDisabled).toBe(false);
  });
});
