import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

const channels = [
  { id: "aaaaaaaa1111", agent_name: "Alice", channel_type: "telegram", display_name: "TG Bot", config: {}, status: "connected", error_msg: null },
  { id: "bbbbbbbb2222", agent_name: "Alice", channel_type: "discord", display_name: "DC Bot", config: {}, status: "error", error_msg: "bad token" },
];

// Only the first channel is "online" (present in the active list).
const active = [
  { agent_name: "Alice", channel_id: "aaaaaaaa1111", channel_type: "telegram", display_name: "TG Bot", adapter_version: "1", connected_at: "", last_activity: "" },
];

vi.mock("@/lib/queries", () => ({
  useChannels: () => ({ data: channels, isLoading: false, error: null }),
  useActiveChannels: () => ({ data: active }),
}));
vi.mock("@tanstack/react-query", () => ({ useQueryClient: () => ({ invalidateQueries: vi.fn() }) }));
vi.mock("@/lib/api", () => ({ apiPost: vi.fn(), apiDelete: vi.fn(), apiPut: vi.fn(), apiGet: vi.fn() }));
vi.mock("@/hooks/use-ws-subscription", () => ({ useWsSubscription: vi.fn() }));
vi.mock("@/stores/auth-store", () => ({
  useAuthStore: (sel: (s: { agents: string[] }) => unknown) => sel({ agents: ["Alice", "Bob"] }),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() }) }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import ChannelsPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("online channel renders a success-tone StatusBadge", () => {
  render(<ChannelsPage />);
  const badge = screen.getByText("channels.online").closest("[data-slot='badge']");
  expect(badge).toHaveAttribute("data-variant", "success");
});

test("errored channel renders a destructive-tone StatusBadge", () => {
  render(<ChannelsPage />);
  const badge = screen.getByText("channels.status_error").closest("[data-slot='badge']");
  expect(badge).toHaveAttribute("data-variant", "destructive");
});

test("Save is disabled until required fields are filled", () => {
  render(<ChannelsPage />);
  // Open the create dialog (telegram default → requires bot_token + display name).
  fireEvent.click(screen.getByText("channels.add"));
  const save = screen.getByRole("button", { name: "common.save" });
  // Agent is auto-selected but name + required config are empty → disabled.
  expect(save).toBeDisabled();
});
