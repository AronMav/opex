import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";

// Config query hook: the page loads config via apiGet("/api/config") plus
// /api/channels and /api/watchdog/settings. Return a small config payload with
// a couple of sections + a top-level scalar so section cards render.
const configPayload = {
  version: "0.2.0",
  subagents: { enabled: true, max_depth: 1 },
  limits: { max_requests_per_minute: 300, max_tool_concurrency: 10 },
};

const apiGet = vi.fn((path: string) => {
  if (path === "/api/config") return Promise.resolve(configPayload);
  if (path === "/api/config/schema") return Promise.resolve({});
  if (path === "/api/channels") return Promise.resolve({ channels: [] });
  if (path === "/api/watchdog/settings")
    return Promise.resolve({ alert_channel_ids: [], alert_events: ["down"] });
  return Promise.resolve({});
});
const apiPost = vi.fn(() => Promise.resolve({}));
const apiPut = vi.fn(() => Promise.resolve({}));

vi.mock("@/lib/api", () => ({
  apiGet: (...args: unknown[]) => apiGet(...(args as [string])),
  apiPost: () => apiPost(),
  apiPut: () => apiPut(),
}));
vi.mock("@/lib/queries", () => ({
  useAgents: () => ({ data: [] }),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() }),
}));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import ConfigPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("renders config section cards after load", async () => {
  render(<ConfigPage />);
  // Section-card titles render (editable-fields + curator + alerting + general
  // are always present; subagents + limits come from the payload).
  expect(await screen.findByText("config.subagents")).toBeInTheDocument();
  expect(screen.getByText("config.editable_fields")).toBeInTheDocument();
  expect(screen.getByText("config.section_alerting")).toBeInTheDocument();
  // A section derived from the config payload renders its raw key as the title.
  expect(screen.getByText("limits")).toBeInTheDocument();
});

test("Restart Core opens a ConfirmDialog and does NOT call the restart API on click", async () => {
  render(<ConfigPage />);
  await screen.findByText("config.editable_fields");

  // Restart button in the header.
  fireEvent.click(screen.getByRole("button", { name: "config.restart_core" }));

  // ConfirmDialog is open (guarded action) — the restart API must NOT have fired.
  const dialog = await screen.findByRole("alertdialog");
  expect(within(dialog).getByText("config.restart_confirm_title")).toBeInTheDocument();
  const confirmBtn = within(dialog).getByRole("button", { name: "config.restart_confirm_action" });
  expect(confirmBtn).toHaveAttribute("data-variant", "destructive");

  // The POST /api/restart was NOT called merely by clicking the header button.
  expect(apiPost).not.toHaveBeenCalled();

  // Confirming fires the restart (POST /api/restart).
  fireEvent.click(confirmBtn);
  await waitFor(() => expect(apiPost).toHaveBeenCalledTimes(1));
});
