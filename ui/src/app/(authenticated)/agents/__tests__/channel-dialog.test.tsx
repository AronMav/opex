import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

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

// ── Test suite ───────────────────────────────────────────────────────────────

describe("ChannelDialog", () => {
  let ChannelDialog: React.ComponentType<any>;

  beforeEach(async () => {
    const mod = await import("../AgentEditDialog");
    ChannelDialog = mod.ChannelDialog;
  });

  function makeProps(overrides: Record<string, unknown> = {}) {
    return {
      open: true,
      onOpenChange: vi.fn(),
      channelDialogId: null,
      channelForm: {
        channel_type: "telegram",
        display_name: "",
        bot_token: "",
        api_url: "",
      },
      setChannelForm: vi.fn(),
      channelSaving: false,
      onSave: vi.fn(),
      ...overrides,
    };
  }

  // ── Test 1: display_name Input onChange ────────────────────────────────────

  it("display_name Input: fireEvent.change → setChannelForm called", () => {
    const setChannelForm = vi.fn();
    const props = makeProps({ setChannelForm });
    render(<ChannelDialog {...props} />);

    const displayNameInput = screen.getByPlaceholderText(/agents\.channel_placeholder_name/i);
    fireEvent.change(displayNameInput, { target: { value: "MyChannel" } });

    expect(setChannelForm).toHaveBeenCalled();
  });

  // ── Test 2: bot_token Input onChange ───────────────────────────────────────

  it("bot_token Input: fireEvent.change → setChannelForm called", () => {
    const setChannelForm = vi.fn();
    const props = makeProps({ setChannelForm });
    render(<ChannelDialog {...props} />);

    const botTokenInput = screen.getByPlaceholderText(/5092.*AAE/i);
    fireEvent.change(botTokenInput, { target: { value: "123:ABC_XYZ" } });

    expect(setChannelForm).toHaveBeenCalled();
  });

  // ── Test 3: api_url Input onChange ────────────────────────────────────────

  it("api_url Input: fireEvent.change → setChannelForm called", () => {
    const setChannelForm = vi.fn();
    const props = makeProps({ setChannelForm });
    render(<ChannelDialog {...props} />);

    const apiUrlInput = screen.getByPlaceholderText(/http:\/\/localhost:8081/i);
    fireEvent.change(apiUrlInput, { target: { value: "http://example.com:8081" } });

    expect(setChannelForm).toHaveBeenCalled();
  });

  // ── Test 4: Save button disabled when display_name empty ───────────────────

  it("Save button disabled when display_name: '' (both empty)", () => {
    const props = makeProps({
      channelForm: {
        channel_type: "telegram",
        display_name: "",
        bot_token: "",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create|agents\.channel_save_and_restart/i });
    expect(saveButton).toBeDisabled();
  });

  // ── Test 5: Save button disabled when bot_token empty ──────────────────────

  it("Save button disabled when display_name: 'MyBot' but bot_token: '' (only bot_token empty)", () => {
    const props = makeProps({
      channelForm: {
        channel_type: "telegram",
        display_name: "MyBot",
        bot_token: "",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create|agents\.channel_save_and_restart/i });
    expect(saveButton).toBeDisabled();
  });

  // ── Test 6: Save button disabled when display_name empty ──────────────────

  it("Save button disabled when bot_token: '123:ABC' but display_name: '' (only display_name empty)", () => {
    const props = makeProps({
      channelForm: {
        channel_type: "telegram",
        display_name: "",
        bot_token: "123:ABC",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create|agents\.channel_save_and_restart/i });
    expect(saveButton).toBeDisabled();
  });

  // ── Test 7: Save button enabled and calls onSave ────────────────────────────

  it("Save button enabled when display_name: 'MyBot' and bot_token: '123:ABC' → onSave called on click", () => {
    const onSave = vi.fn();
    const props = makeProps({
      channelForm: {
        channel_type: "telegram",
        display_name: "MyBot",
        bot_token: "123:ABC",
        api_url: "",
      },
      onSave,
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create|agents\.channel_save_and_restart/i });
    expect(saveButton).not.toBeDisabled();

    fireEvent.click(saveButton);
    expect(onSave).toHaveBeenCalled();
  });

  // ── Test 8: channel_type Select disabled when editing ──────────────────────

  it("channelDialogId: 'some-id' → channel_type Select disabled", () => {
    const props = makeProps({
      channelDialogId: "existing-channel-id",
      channelForm: {
        channel_type: "telegram",
        display_name: "ExistingBot",
        bot_token: "123:ABC",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const typeSelects = screen.getAllByRole("combobox");
    expect(typeSelects[0]).toBeDisabled();
  });

  // ── Test 9: Dialog title for creating new channel ────────────────────────────

  it("channelDialogId: null → Dialog title is 'agents.channel_add_dialog'", () => {
    const props = makeProps({ channelDialogId: null });
    render(<ChannelDialog {...props} />);

    expect(screen.getByText(/agents\.channel_add_dialog/i)).toBeInTheDocument();
  });

  // ── Test 10: Dialog title for editing existing channel ────────────────────────

  it("channelDialogId: 'id' → Dialog title is 'agents.channel_edit'", () => {
    const props = makeProps({ channelDialogId: "existing-id" });
    render(<ChannelDialog {...props} />);

    expect(screen.getByText(/agents\.channel_edit/i)).toBeInTheDocument();
  });

  // ── Test 11: Cancel button closes dialog ──────────────────────────────────────

  it("Cancel button calls onOpenChange(false)", () => {
    const onOpenChange = vi.fn();
    const props = makeProps({ onOpenChange });
    render(<ChannelDialog {...props} />);

    const cancelButton = screen.getByRole("button", { name: /common\.cancel/i });
    fireEvent.click(cancelButton);

    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  // ── Test 12: Save button disabled when channelSaving ────────────────────────

  it("Save button disabled when channelSaving: true", () => {
    const props = makeProps({
      channelForm: {
        channel_type: "telegram",
        display_name: "MyBot",
        bot_token: "123:ABC",
        api_url: "",
      },
      channelSaving: true,
    });
    render(<ChannelDialog {...props} />);

    const buttons = screen.getAllByRole("button");
    const saveButton = buttons.find(b => b.textContent && /saving|create|save_and_restart/i.test(b.textContent));
    expect(saveButton).toBeDisabled();
  });

  // ── Test 13: display_name Input calls setChannelForm with updater function ────

  it("display_name Input onChange → setChannelForm called with function argument", () => {
    const setChannelForm = vi.fn();
    const props = makeProps({ setChannelForm });
    render(<ChannelDialog {...props} />);

    const displayNameInput = screen.getByPlaceholderText(/agents\.channel_placeholder_name/i);
    fireEvent.change(displayNameInput, { target: { value: "TestName" } });

    expect(setChannelForm).toHaveBeenCalled();
    const arg = setChannelForm.mock.calls[0][0];
    expect(typeof arg).toBe("function");
  });

  // ── Test 14: bot_token Input is type="password" ─────────────────────────────

  it("bot_token Input has type='password'", () => {
    const props = makeProps();
    render(<ChannelDialog {...props} />);

    const botTokenInput = screen.getByPlaceholderText(/5092.*AAE/i) as HTMLInputElement;
    expect(botTokenInput.type).toBe("password");
  });

  // ── Test 15: api_url Input calls setChannelForm with updater function ─────────

  it("api_url Input onChange → setChannelForm called with function argument", () => {
    const setChannelForm = vi.fn();
    const props = makeProps({ setChannelForm });
    render(<ChannelDialog {...props} />);

    const apiUrlInput = screen.getByPlaceholderText(/http:\/\/localhost:8081/i);
    fireEvent.change(apiUrlInput, { target: { value: "http://new-url.com" } });

    expect(setChannelForm).toHaveBeenCalled();
    const arg = setChannelForm.mock.calls[0][0];
    expect(typeof arg).toBe("function");
  });

  // ── Test 16: Save button label "agents.channel_save_and_restart" when editing ─

  it("Save button label is 'agents.channel_save_and_restart' when editing (channelDialogId !== null)", () => {
    const props = makeProps({
      channelDialogId: "existing-id",
      channelForm: {
        channel_type: "telegram",
        display_name: "MyBot",
        bot_token: "123:ABC",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /agents\.channel_save_and_restart/i });
    expect(saveButton).toBeInTheDocument();
  });

  // ── Test 17: Save button label "common.create" when creating ──────────────────

  it("Save button label is 'common.create' when creating (channelDialogId === null)", () => {
    const props = makeProps({
      channelDialogId: null,
      channelForm: {
        channel_type: "telegram",
        display_name: "MyBot",
        bot_token: "123:ABC",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create/i });
    expect(saveButton).toBeInTheDocument();
  });

  // ── Test 18: Trimmed display_name validation ──────────────────────────────────

  it("Save button disabled when display_name: '   ' (whitespace only)", () => {
    const props = makeProps({
      channelForm: {
        channel_type: "telegram",
        display_name: "   ",
        bot_token: "123:ABC",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create|agents\.channel_save_and_restart/i });
    expect(saveButton).toBeDisabled();
  });

  // ── Test 19: Trimmed bot_token validation ────────────────────────────────────

  it("Save button disabled when bot_token: '   ' (whitespace only)", () => {
    const props = makeProps({
      channelForm: {
        channel_type: "telegram",
        display_name: "MyBot",
        bot_token: "   ",
        api_url: "",
      },
    });
    render(<ChannelDialog {...props} />);

    const saveButton = screen.getByRole("button", { name: /common\.create|agents\.channel_save_and_restart/i });
    expect(saveButton).toBeDisabled();
  });

  // ── Test 20: channel_type Select not disabled when creating ──────────────────

  it("channelDialogId: null → channel_type Select not disabled", () => {
    const props = makeProps({ channelDialogId: null });
    render(<ChannelDialog {...props} />);

    const typeSelects = screen.getAllByRole("combobox");
    expect(typeSelects[0]).not.toBeDisabled();
  });

  // ── Test 21: Multiple field changes in sequence ────────────────────────────────

  it("Multiple Input changes in sequence all call setChannelForm", () => {
    const setChannelForm = vi.fn();
    const props = makeProps({ setChannelForm });
    render(<ChannelDialog {...props} />);

    const displayNameInput = screen.getByPlaceholderText(/agents\.channel_placeholder_name/i);
    const botTokenInput = screen.getByPlaceholderText(/5092.*AAE/i);
    const apiUrlInput = screen.getByPlaceholderText(/http:\/\/localhost:8081/i);

    fireEvent.change(displayNameInput, { target: { value: "Bot1" } });
    fireEvent.change(botTokenInput, { target: { value: "token1" } });
    fireEvent.change(apiUrlInput, { target: { value: "http://url1" } });

    expect(setChannelForm).toHaveBeenCalledTimes(3);
  });

  // ── Test 22: Dialog renders all form fields ─────────────────────────────────

  it("Dialog renders all form fields: channel_type, display_name, bot_token, api_url", () => {
    const props = makeProps();
    render(<ChannelDialog {...props} />);

    expect(screen.getByText(/agents\.channel_field_type/i)).toBeInTheDocument();
    expect(screen.getByText(/agents\.channel_field_display_name/i)).toBeInTheDocument();
    expect(screen.getByText(/agents\.channel_field_bot_token/i)).toBeInTheDocument();
    expect(screen.getByText(/agents\.channel_field_api_url/i)).toBeInTheDocument();
  });
});
