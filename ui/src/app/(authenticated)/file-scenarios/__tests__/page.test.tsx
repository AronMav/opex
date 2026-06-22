import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import type { FileScenario } from "@/types/api";

const setDefaultMutate = vi.fn();
const updateMutate = vi.fn();

const scenarios: FileScenario[] = [
  { id: "img-d", match_type: "image/*", executor: "tool", action_ref: "describe",
    label: "Describe image", is_default: true, priority: 100, enabled: true,
    scope: "global", created_by: "system",
    created_at: "2026-06-22T00:00:00Z", updated_at: "2026-06-22T00:00:00Z" },
  { id: "aud-t", match_type: "audio/*", executor: "tool", action_ref: "transcribe",
    label: "Transcribe", is_default: true, priority: 100, enabled: true,
    scope: "global", created_by: "system",
    created_at: "2026-06-22T00:00:00Z", updated_at: "2026-06-22T00:00:00Z" },
];

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
vi.mock("@/lib/queries", () => ({
  useFileScenarios: () => ({ data: scenarios, isLoading: false, error: null, refetch: vi.fn() }),
  useCreateFileScenario: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useUpdateFileScenario: () => ({ mutate: updateMutate, mutateAsync: vi.fn() }),
  useDeleteFileScenario: () => ({ mutate: vi.fn() }),
  useSetFileScenarioDefault: () => ({ mutate: setDefaultMutate }),
  useFileScenarioAllowlist: () => ({ data: [] }),
  useSetFileScenarioAllowlist: () => ({ mutate: vi.fn() }),
}));

import FileScenariosPage from "../page";

describe("FileScenariosPage", () => {
  beforeEach(() => { setDefaultMutate.mockClear(); updateMutate.mockClear(); });

  it("renders one group per match_type with its bindings", () => {
    render(<FileScenariosPage />);
    expect(screen.getByText("image/*")).toBeInTheDocument();
    expect(screen.getByText("audio/*")).toBeInTheDocument();
    expect(screen.getByText("Describe image")).toBeInTheDocument();
    expect(screen.getByText("Transcribe")).toBeInTheDocument();
  });

  it("toggles default off via useSetFileScenarioDefault", () => {
    render(<FileScenariosPage />);
    // image group default star (already default) → clicking unsets it
    const stars = screen.getAllByRole("button", { name: /file_scenarios.set_default/i });
    fireEvent.click(stars[0]);
    expect(setDefaultMutate).toHaveBeenCalledWith(
      expect.objectContaining({ id: expect.any(String), is_default: false }),
    );
  });

  it("toggles enabled via useUpdateFileScenario", () => {
    render(<FileScenariosPage />);
    const switches = screen.getAllByLabelText("file_scenarios.toggle_enabled");
    fireEvent.click(switches[0]);
    expect(updateMutate).toHaveBeenCalledWith(
      expect.objectContaining({ id: expect.any(String), enabled: false }),
    );
  });
});
