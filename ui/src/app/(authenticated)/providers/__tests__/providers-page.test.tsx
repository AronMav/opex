import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { toast } from "sonner";

// A capability provider (stt) that is present in the active list, and a
// disabled one that is not — exercises active/inactive split + StatusBadge.
// `let` (not `const`): a couple of tests below swap these in/out per-case
// (embedding-only active group, delete-conflict) — the mocked hooks below
// read the current value at render time via closure.
const defaultProviders = [
  { id: "p1", name: "local-whisper", type: "stt", provider_type: "whisper", enabled: true, base_url: "http://x/v1", has_api_key: false },
  { id: "p2", name: "cloud-whisper", type: "stt", provider_type: "openai", enabled: false, has_api_key: true },
];
const defaultActive = [
  { capability: "stt", provider_name: "local-whisper", priority: 1 },
];
let providers = defaultProviders;
let active = defaultActive;

const mutation = () => ({ mutate: vi.fn(), mutateAsync: vi.fn() });

// Reassigned per-test so the delete-error path can be exercised without a
// full mock re-wire (see the "delete_in_profiles" tests below).
let deleteMutate = vi.fn();

vi.mock("@/lib/queries", () => ({
  useProviders: () => ({ data: providers, isLoading: false, error: null, refetch: vi.fn() }),
  useProviderTypes: () => ({ data: [] }),
  useProviderActive: () => ({ data: active }),
  useProviderModelsDetailed: () => ({ data: [], isLoading: false }),
  useMediaDrivers: () => ({ data: {} }),
  useCreateProvider: () => mutation(),
  useUpdateProvider: () => mutation(),
  useDeleteProvider: () => ({ mutate: deleteMutate, mutateAsync: vi.fn() }),
  useSetProviderActive: () => mutation(),
}));
vi.mock("@/lib/api", () => ({ apiGet: vi.fn(), apiPost: vi.fn() }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() }) }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string, vars?: Record<string, unknown>) => (vars ? `${k}:${JSON.stringify(vars)}` : k), locale: "en" }),
}));

import ProvidersPage from "../page";

beforeEach(() => {
  vi.clearAllMocks();
  deleteMutate = vi.fn();
  providers = defaultProviders;
  active = defaultActive;
});

test("renders a row for each provider", () => {
  render(<ProvidersPage />);
  expect(screen.getByText("local-whisper")).toBeInTheDocument();
  expect(screen.getByText("cloud-whisper")).toBeInTheDocument();
});

test("enabled provider row shows a success-tone StatusBadge", () => {
  render(<ProvidersPage />);
  // The enabled label appears at least once; its badge carries the success variant.
  const badge = screen.getAllByText("providers.status_enabled")[0].closest("[data-slot='badge']");
  expect(badge).toHaveAttribute("data-variant", "success");
});

test("disabled provider row shows a secondary-tone StatusBadge", () => {
  render(<ProvidersPage />);
  const badge = screen.getByText("providers.status_disabled").closest("[data-slot='badge']");
  expect(badge).toHaveAttribute("data-variant", "secondary");
});

// ── Task 18: active-provider groups restricted to `embedding` ───────────────

test("stt tab does NOT render an active-toggle switch (profiles now own stt routing)", () => {
  render(<ProvidersPage />);
  // stt is the first visible category (defaultValue) so its tab is already open.
  expect(screen.queryAllByRole("switch")).toHaveLength(0);
});

test("embedding tab renders an active-toggle switch (the sole remaining global capability)", () => {
  // Only an embedding provider present → its tab is the sole visible one and
  // is active by default, sidestepping simulated Radix tab-switch clicks.
  providers = [
    { id: "p3", name: "local-embed", type: "embedding", provider_type: "ollama", enabled: true, has_api_key: false },
  ];
  active = [{ capability: "embedding", provider_name: "local-embed", priority: 1 }];
  render(<ProvidersPage />);
  expect(screen.getByText("providers.embedding_section")).toBeInTheDocument();
  expect(screen.getAllByRole("switch").length).toBeGreaterThan(0);
});

// ── Task 18: delete 409 "provider_in_profiles" toast ────────────────────────

test("delete of a profile-referenced provider shows the friendly conflict toast, not the raw error", () => {
  deleteMutate = vi.fn((_id: string, opts: { onError: (e: Error) => void }) => {
    opts.onError(Object.assign(new Error("provider_in_profiles"), {
      body: { error: "provider_in_profiles", profiles: ["Assistant", "Support"] },
    }));
  });
  render(<ProvidersPage />);
  // Two provider rows each have their own delete button; click the first to
  // open the confirm dialog, then the dialog's own confirm action (appended
  // last in document order via its Portal).
  fireEvent.click(screen.getAllByRole("button", { name: "common.delete" })[0]);
  const confirmButtons = screen.getAllByRole("button", { name: "common.delete" });
  fireEvent.click(confirmButtons[confirmButtons.length - 1]);
  expect(toast.error).toHaveBeenCalledWith(
    expect.stringContaining("providers.delete_in_profiles"),
  );
  expect(toast.error).toHaveBeenCalledWith(
    expect.stringContaining("Assistant, Support"),
  );
});

test("delete failing for another reason still shows the generic delete_error toast", () => {
  deleteMutate = vi.fn((_id: string, opts: { onError: (e: Error) => void }) => {
    opts.onError(new Error("network down"));
  });
  render(<ProvidersPage />);
  fireEvent.click(screen.getAllByRole("button", { name: "common.delete" })[0]);
  const confirmButtons = screen.getAllByRole("button", { name: "common.delete" });
  fireEvent.click(confirmButtons[confirmButtons.length - 1]);
  expect(toast.error).toHaveBeenCalledWith(
    expect.stringContaining("providers.delete_error"),
  );
  expect(toast.error).not.toHaveBeenCalledWith(
    expect.stringContaining("providers.delete_in_profiles"),
  );
});
