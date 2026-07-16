import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

import { WebhooksEditor } from "../AgentEditDialog";
import type { WebhookDto } from "@/types/api.generated";

// ── Mocks ────────────────────────────────────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

// ── Helpers ───────────────────────────────────────────────────────────────────

function makeWebhook(overrides: Partial<WebhookDto> = {}): WebhookDto {
  return {
    url: "",
    events: [],
    mode: "async",
    tool_matcher: null,
    on_failure: "open",
    timeout_ms: 3000,
    allow_internal: false,
    ...overrides,
  };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("WebhooksEditor", () => {
  it("renders empty state with Add button", () => {
    const onChange = vi.fn();
    render(<WebhooksEditor webhooks={[]} onChange={onChange} />);
    expect(screen.getByText(/Добавить webhook/i)).toBeInTheDocument();
  });

  it("clicking Add webhook adds a new entry with defaults", () => {
    const onChange = vi.fn();
    render(<WebhooksEditor webhooks={[]} onChange={onChange} />);
    fireEvent.click(screen.getByText(/Добавить webhook/i));
    expect(onChange).toHaveBeenCalledWith([makeWebhook()]);
  });

  it("url input change updates webhook url", () => {
    const onChange = vi.fn();
    render(<WebhooksEditor webhooks={[makeWebhook()]} onChange={onChange} />);
    const urlInput = screen.getByPlaceholderText(/https:\/\//i);
    fireEvent.change(urlInput, { target: { value: "https://example.com/hook" } });
    expect(onChange).toHaveBeenCalledWith([makeWebhook({ url: "https://example.com/hook" })]);
  });

  it("delete button removes the webhook", () => {
    const onChange = vi.fn();
    render(<WebhooksEditor webhooks={[makeWebhook({ url: "https://x.com" })]} onChange={onChange} />);
    const deleteBtn = screen.getByRole("button", { name: /удалить/i });
    fireEvent.click(deleteBtn);
    expect(onChange).toHaveBeenCalledWith([]);
  });

  it("decision mode toggle shows on_failure and timeout fields", () => {
    const onChange = vi.fn();
    render(
      <WebhooksEditor
        webhooks={[makeWebhook({ mode: "decision" })]}
        onChange={onChange}
      />
    );
    expect(screen.getByText(/on_failure/i)).toBeInTheDocument();
    expect(screen.getByText(/timeout/i)).toBeInTheDocument();
  });

  it("async mode hides decision-only fields", () => {
    const onChange = vi.fn();
    render(
      <WebhooksEditor
        webhooks={[makeWebhook({ mode: "async" })]}
        onChange={onChange}
      />
    );
    expect(screen.queryByText(/on_failure/i)).not.toBeInTheDocument();
  });

  it("BeforeMessage event toggle adds event to events array", () => {
    const onChange = vi.fn();
    render(<WebhooksEditor webhooks={[makeWebhook({ events: [] })]} onChange={onChange} />);
    const checkbox = screen.getByRole("checkbox", { name: /BeforeMessage/i });
    fireEvent.click(checkbox);
    expect(onChange).toHaveBeenCalledWith([makeWebhook({ events: ["BeforeMessage"] })]);
  });

  it("BeforeToolCall event toggle adds event to events array", () => {
    const onChange = vi.fn();
    render(<WebhooksEditor webhooks={[makeWebhook({ events: [] })]} onChange={onChange} />);
    const checkbox = screen.getByRole("checkbox", { name: /BeforeToolCall/i });
    fireEvent.click(checkbox);
    expect(onChange).toHaveBeenCalledWith([makeWebhook({ events: ["BeforeToolCall"] })]);
  });

  it("AfterToolResult event toggle adds event to events array", () => {
    const onChange = vi.fn();
    render(<WebhooksEditor webhooks={[makeWebhook({ events: [] })]} onChange={onChange} />);
    const checkbox = screen.getByRole("checkbox", { name: /AfterToolResult/i });
    fireEvent.click(checkbox);
    expect(onChange).toHaveBeenCalledWith([makeWebhook({ events: ["AfterToolResult"] })]);
  });

  it("unchecking an event removes it from events array", () => {
    const onChange = vi.fn();
    render(
      <WebhooksEditor
        webhooks={[makeWebhook({ events: ["BeforeMessage", "BeforeToolCall"] })]}
        onChange={onChange}
      />
    );
    const checkbox = screen.getByRole("checkbox", { name: /BeforeMessage/i });
    fireEvent.click(checkbox);
    expect(onChange).toHaveBeenCalledWith([makeWebhook({ events: ["BeforeToolCall"] })]);
  });

  it("tool_matcher null renders as empty string", () => {
    const onChange = vi.fn();
    render(
      <WebhooksEditor
        webhooks={[makeWebhook({ mode: "decision", tool_matcher: null })]}
        onChange={onChange}
      />
    );
    const input = screen.getByPlaceholderText(/tool_matcher/i);
    expect((input as HTMLInputElement).value).toBe("");
  });

  it("tool_matcher input empty string → null in onChange", () => {
    const onChange = vi.fn();
    render(
      <WebhooksEditor
        webhooks={[makeWebhook({ mode: "decision", tool_matcher: "old" })]}
        onChange={onChange}
      />
    );
    const input = screen.getByPlaceholderText(/tool_matcher/i);
    fireEvent.change(input, { target: { value: "" } });
    expect(onChange).toHaveBeenCalledWith([makeWebhook({ mode: "decision", tool_matcher: null })]);
  });

  it("allow_internal switch toggle updates value", () => {
    const onChange = vi.fn();
    render(
      <WebhooksEditor
        webhooks={[makeWebhook({ mode: "decision", allow_internal: false })]}
        onChange={onChange}
      />
    );
    const sw = screen.getByRole("switch");
    fireEvent.click(sw);
    expect(onChange).toHaveBeenCalledWith([makeWebhook({ mode: "decision", allow_internal: true })]);
  });
});

// ── formToPayload webhook tests ───────────────────────────────────────────────

describe("formToPayload — webhooks", () => {
  it("hooksWebhooks always included in hooks payload when present", async () => {
    const { formToPayload, emptyForm } = await import("../page");
    const wh = makeWebhook({ url: "https://x.com", events: ["BeforeMessage"] });
    const payload = formToPayload({ ...emptyForm, hooksWebhooks: [wh] });
    expect(payload.hooks).not.toBeNull();
    expect((payload.hooks as { webhooks: WebhookDto[] }).webhooks).toEqual([wh]);
  });

  it("hooksWebhooks: [] → hooks null when logAll=false and blockTools empty", async () => {
    const { formToPayload, emptyForm } = await import("../page");
    const payload = formToPayload({ ...emptyForm, hooksWebhooks: [] });
    expect(payload.hooks).toBeNull();
  });

  it("webhooks always in hooks payload when logAll=true", async () => {
    const { formToPayload, emptyForm } = await import("../page");
    const payload = formToPayload({ ...emptyForm, hooksLogAll: true, hooksWebhooks: [] });
    expect(payload.hooks).not.toBeNull();
    expect((payload.hooks as { webhooks: WebhookDto[] }).webhooks).toEqual([]);
  });
});

// ── detailToForm webhook init tests ──────────────────────────────────────────

describe("detailToForm — webhooks init", () => {
  it("hooks: null → hooksWebhooks: []", async () => {
    const { detailToForm } = await import("../page");
    const form = detailToForm({
      name: "A", language: "ru", profile: "Default",
      capabilities: { text: true, stt: false, tts: false, vision: false, imagegen: false, websearch: false },
      temperature: 1, max_tokens: null,
      access: null, heartbeat: null, tools: null, compaction: null,
      skill_review: null, session: null, icon_url: null,
      max_tools_in_context: null, tool_loop: null, tool_dispatcher: null,
      approval: null, routing: [], watchdog: null, hooks: null,
      max_history_messages: null, daily_budget_tokens: 0,
      max_failover_attempts: 0, is_running: false, config_dirty: false,
      soul: { enabled: false, reflection_threshold: 150, reflection_cooldown_minutes: 60, context_top_k: 6, context_budget_tokens: 800, max_events_per_session: 10 },
      drift: { enabled: false, threshold: 0.15, min_history: 6, baseline_turns: 3, correct: false },
      initiative: { enabled: false, daily_proposal_cap: 1, decompose: false, daily_plan: false, auto_approve_day_plan: false, daily_token_budget: 0 },
      emotion: { enabled: false, intensity_importance_k: 3, blend_rate: 0.3, decay_half_life_hours: 12 },
    });
    expect(form.hooksWebhooks).toEqual([]);
  });

  it("hooks.webhooks populated → hooksWebhooks set", async () => {
    const { detailToForm } = await import("../page");
    const wh = makeWebhook({ url: "https://h.com" });
    const form = detailToForm({
      name: "A", language: "ru", profile: "Default",
      capabilities: { text: true, stt: false, tts: false, vision: false, imagegen: false, websearch: false },
      temperature: 1, max_tokens: null,
      access: null, heartbeat: null, tools: null, compaction: null,
      skill_review: null, session: null, icon_url: null,
      max_tools_in_context: null, tool_loop: null, tool_dispatcher: null,
      approval: null, routing: [], watchdog: null,
      hooks: { log_all_tool_calls: false, block_tools: [], webhooks: [wh] },
      max_history_messages: null, daily_budget_tokens: 0,
      max_failover_attempts: 0, is_running: false, config_dirty: false,
      soul: { enabled: false, reflection_threshold: 150, reflection_cooldown_minutes: 60, context_top_k: 6, context_budget_tokens: 800, max_events_per_session: 10 },
      drift: { enabled: false, threshold: 0.15, min_history: 6, baseline_turns: 3, correct: false },
      initiative: { enabled: false, daily_proposal_cap: 1, decompose: false, daily_plan: false, auto_approve_day_plan: false, daily_token_budget: 0 },
      emotion: { enabled: false, intensity_importance_k: 3, blend_rate: 0.3, decay_half_life_hours: 12 },
    });
    expect(form.hooksWebhooks).toEqual([wh]);
  });
});
