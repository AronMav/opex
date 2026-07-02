import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

const secrets = [
  { name: "OPENAI_KEY", scope: "", description: "primary key", has_value: true, created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-02T00:00:00Z" },
  { name: "EMPTY_KEY", scope: "Alice", description: null, has_value: false, created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-02T00:00:00Z" },
];

vi.mock("@/lib/queries", () => ({
  useSecrets: () => ({ data: secrets, isLoading: false, error: null, refetch: vi.fn() }),
  useAgents: () => ({ data: [{ name: "Alice" }, { name: "Bob" }] }),
  useUpsertSecret: () => ({ mutateAsync: vi.fn(), isPending: false, error: null }),
  useDeleteSecret: () => ({ mutateAsync: vi.fn(), isPending: false, error: null }),
}));
vi.mock("@/lib/api", () => ({ apiGet: vi.fn(() => Promise.resolve({ name: "OPENAI_KEY", value: "sk-123" })) }));
vi.mock("@/lib/clipboard", () => ({ copyText: vi.fn(() => Promise.resolve()) }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import SecretsPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("secret with a value renders a success-tone StatusBadge", () => {
  render(<SecretsPage />);
  const badge = screen.getByText("secrets.active");
  expect(badge).toHaveAttribute("data-variant", "success");
});

test("empty secret renders a secondary-tone StatusBadge", () => {
  render(<SecretsPage />);
  const badge = screen.getByText("secrets.empty");
  expect(badge).toHaveAttribute("data-variant", "secondary");
});

test("renders the migrated secret rows (name + scope badge)", () => {
  render(<SecretsPage />);
  expect(screen.getByText("OPENAI_KEY")).toBeInTheDocument();
  expect(screen.getByText("EMPTY_KEY")).toBeInTheDocument();
  // scope badge (outline-primary variant) is rendered for the scoped secret
  const scopeBadge = screen.getByText("Alice");
  expect(scopeBadge).toHaveAttribute("data-variant", "outline-primary");
});
