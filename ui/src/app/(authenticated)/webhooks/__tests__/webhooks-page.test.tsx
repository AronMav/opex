import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

const webhooks = [
  { id: "1", name: "hook-a", agent_id: "Alice", webhook_type: "generic", enabled: true, trigger_count: 3, created_at: "2026-01-01T00:00:00Z", last_triggered_at: null, event_filter: [], prompt_prefix: "" },
  { id: "2", name: "hook-b", agent_id: "Bob", webhook_type: "github", enabled: false, trigger_count: 0, created_at: "2026-01-01T00:00:00Z", last_triggered_at: null, event_filter: [], prompt_prefix: "" },
];

vi.mock("@/lib/queries", () => ({
  useWebhooks: () => ({ data: webhooks, isLoading: false, error: null }),
  useAgents: () => ({ data: [{ name: "Alice" }, { name: "Bob" }] }),
  useCreateWebhook: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useUpdateWebhook: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useDeleteWebhook: () => ({ mutateAsync: vi.fn(), isPending: false }),
}));
vi.mock("@tanstack/react-query", () => ({ useQueryClient: () => ({ invalidateQueries: vi.fn() }) }));
vi.mock("@/lib/api", () => ({ apiPost: vi.fn(() => Promise.resolve({ secret: "s" })) }));
vi.mock("@/lib/clipboard", () => ({ copyText: vi.fn(() => Promise.resolve()) }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import WebhooksPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("enabled webhook shows a success-tone status badge", () => {
  render(<WebhooksPage />);
  const badge = screen.getAllByText("common.enabled")[0];
  expect(badge).toHaveAttribute("data-variant", "success");
});

test("disabled webhook shows a secondary-tone status badge", () => {
  render(<WebhooksPage />);
  const badge = screen.getAllByText("common.disabled")[0];
  expect(badge).toHaveAttribute("data-variant", "secondary");
});

test("regenerate secret opens a confirmation before firing", () => {
  render(<WebhooksPage />);
  fireEvent.click(screen.getAllByLabelText("common.regenerate_secret")[0]);
  // ConfirmDialog title is rendered; the network call is NOT made yet.
  expect(screen.getByText("webhooks.regenerate_confirm_title")).toBeInTheDocument();
});
