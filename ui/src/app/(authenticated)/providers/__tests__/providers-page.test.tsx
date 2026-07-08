import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// A capability provider (stt) that is present in the active list, and a
// disabled one that is not — exercises active/inactive split + StatusBadge.
const providers = [
  { id: "p1", name: "local-whisper", type: "stt", provider_type: "whisper", enabled: true, base_url: "http://x/v1", has_api_key: false },
  { id: "p2", name: "cloud-whisper", type: "stt", provider_type: "openai", enabled: false, has_api_key: true },
];

const active = [
  { capability: "stt", provider_name: "local-whisper", priority: 1 },
];

const mutation = () => ({ mutate: vi.fn(), mutateAsync: vi.fn() });

vi.mock("@/lib/queries", () => ({
  useProviders: () => ({ data: providers, isLoading: false, error: null, refetch: vi.fn() }),
  useProviderTypes: () => ({ data: [] }),
  useProviderActive: () => ({ data: active }),
  useProviderModelsDetailed: () => ({ data: [], isLoading: false }),
  useMediaDrivers: () => ({ data: {} }),
  useCreateProvider: () => mutation(),
  useUpdateProvider: () => mutation(),
  useDeleteProvider: () => mutation(),
  useSetProviderActive: () => mutation(),
}));
vi.mock("@/lib/api", () => ({ apiGet: vi.fn(), apiPost: vi.fn() }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() }) }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import ProvidersPage from "../page";

beforeEach(() => vi.clearAllMocks());

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
