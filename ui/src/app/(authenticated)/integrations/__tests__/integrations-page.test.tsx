import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

const accounts = [
  {
    id: "acc-connected",
    provider: "github",
    display_name: "My GitHub",
    user_email: "dev@example.com",
    scope: "",
    status: "connected",
    expires_at: null,
    connected_at: "2026-01-02T00:00:00Z",
    created_at: "2026-01-01T00:00:00Z",
  },
  {
    id: "acc-expired",
    provider: "google",
    display_name: "My Google",
    user_email: null,
    scope: "",
    status: "expired",
    expires_at: "2026-02-01T00:00:00Z",
    connected_at: "2026-01-02T00:00:00Z",
    created_at: "2026-01-01T00:00:00Z",
  },
];

const apiPost = vi.fn().mockResolvedValue({});
const apiDelete = vi.fn().mockResolvedValue({});
const apiGet = vi.fn().mockResolvedValue({ triggers: [], repos: [] });

vi.mock("@/lib/api", () => ({
  apiGet: (...a: unknown[]) => apiGet(...a),
  apiPost: (...a: unknown[]) => apiPost(...a),
  apiDelete: (...a: unknown[]) => apiDelete(...a),
}));
vi.mock("@/lib/queries", () => ({
  useOAuthAccounts: () => ({ data: accounts, isLoading: false }),
  useOAuthBindings: () => ({ data: [] }),
  qk: { oauthAccounts: ["oauth", "accounts"], oauthBindings: (a: string) => ["oauth", "bindings", a] },
}));
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: vi.fn() }),
  useQuery: () => ({ data: [], refetch: vi.fn() }),
}));
vi.mock("next/navigation", () => ({
  useSearchParams: () => new URLSearchParams(""),
}));
vi.mock("@/stores/auth-store", () => ({
  useAuthStore: (sel: (s: { agents: string[] }) => unknown) => sel({ agents: ["Alice", "Bob"] }),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import IntegrationsPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("connected account renders a success-tone StatusBadge", () => {
  render(<IntegrationsPage />);
  const badge = screen.getByText("integrations.status_connected").closest("[data-slot='badge']");
  expect(badge).toHaveAttribute("data-variant", "success");
});

test("expired account renders a warning-tone StatusBadge", () => {
  render(<IntegrationsPage />);
  const badge = screen.getByText("integrations.status_expired").closest("[data-slot='badge']");
  expect(badge).toHaveAttribute("data-variant", "warning");
});

test("account rows render with display names", () => {
  render(<IntegrationsPage />);
  expect(screen.getByText("My GitHub")).toBeInTheDocument();
  expect(screen.getByText("My Google")).toBeInTheDocument();
});

test("clicking Revoke opens a ConfirmDialog and does NOT fire the network call yet", () => {
  render(<IntegrationsPage />);
  fireEvent.click(screen.getByRole("button", { name: "integrations.revoke" }));
  // Confirm dialog is open (alertdialog role) with the revoke title.
  expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  expect(screen.getByText("integrations.revoke_confirm_title")).toBeInTheDocument();
  // The revoke POST must not have been fired just by opening the dialog.
  expect(apiPost).not.toHaveBeenCalled();
});

test("clicking Delete opens a ConfirmDialog and does NOT fire the network call yet", () => {
  render(<IntegrationsPage />);
  // Each account row has a delete (trash) icon button — click the first.
  const deleteButtons = screen.getAllByRole("button", { name: "integrations.delete_account" });
  fireEvent.click(deleteButtons[0]);
  expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  expect(screen.getByText("integrations.delete_confirm_title")).toBeInTheDocument();
  // The delete DELETE must not have been fired just by opening the dialog.
  expect(apiDelete).not.toHaveBeenCalled();
});
