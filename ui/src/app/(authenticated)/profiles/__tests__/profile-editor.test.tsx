import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

import type { ProfileBase } from "@/hooks/use-profiles";

// ── Mocks ────────────────────────────────────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/lib/queries", () => ({
  useProviders: () => ({
    data: [
      { id: "p1", name: "openai", type: "text", provider_type: "openai_compat", default_model: "gpt-4.1", enabled: true },
      { id: "p2", name: "minimax", type: "tts", provider_type: "minimax", default_model: null, enabled: true },
      { id: "p3", name: "other-llm", type: "text", provider_type: "openai_compat", default_model: "glm-5", enabled: true },
    ],
  }),
  useProviderModelsDetailed: () => ({ data: [], isLoading: false }),
  useTtsVoices: () => ({ data: [], isLoading: false, isError: false }),
}));

// jsdom не реализует scrollIntoView/pointer capture, которые дергает Radix Select.
window.HTMLElement.prototype.scrollIntoView = vi.fn();
window.HTMLElement.prototype.hasPointerCapture = vi.fn();
window.HTMLElement.prototype.releasePointerCapture = vi.fn();

const { mockMutate } = vi.hoisted(() => ({ mockMutate: vi.fn() }));
vi.mock("@/hooks/use-profiles", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/hooks/use-profiles")>();
  return {
    ...actual,
    useUpdateProfile: () => ({ mutate: mockMutate }),
  };
});

// ── Fixture ──────────────────────────────────────────────────────────────────

function makeProfile(overrides: Partial<ProfileBase> = {}): ProfileBase {
  return {
    id: "profile-1",
    name: "Voice",
    slots: {
      text: [{ provider: "openai", model: "gpt-4" }],
    },
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

// ── Tests ────────────────────────────────────────────────────────────────────

describe("ProfileEditor", () => {
  let ProfileEditor: React.ComponentType<{
    profile: ProfileBase;
    open: boolean;
    onClose: () => void;
  }>;

  beforeEach(async () => {
    mockMutate.mockClear();
    const mod = await import("../_parts/ProfileEditor");
    ProfileEditor = mod.ProfileEditor;
  });

  it("renders one row for the text slot initially", () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);
    expect(screen.getAllByTestId(/^profile-row-text-/)).toHaveLength(1);
  });

  it("adding a reserve yields a second row for that slot", () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);

    // The "text" capability is rendered first (PROFILE_CAPABILITIES order),
    // so its "+ reserve" button is the first one in the document.
    const addButtons = screen.getAllByRole("button", { name: /profiles\.add_reserve/i });
    fireEvent.click(addButtons[0]);

    expect(screen.getAllByTestId(/^profile-row-text-/)).toHaveLength(2);
  });

  it("the up arrow swaps the reordered row with its predecessor", () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);

    // Add a second row and give it a distinct model value.
    fireEvent.click(screen.getAllByRole("button", { name: /profiles\.add_reserve/i })[0]);
    const modelInputs = () => screen.getAllByTestId(/^profile-model-text-/) as HTMLInputElement[];
    fireEvent.change(modelInputs()[1], { target: { value: "gpt-4o-mini" } });

    expect(modelInputs()[0]).toHaveValue("gpt-4");
    expect(modelInputs()[1]).toHaveValue("gpt-4o-mini");

    // Move the second row up — it should swap with the first.
    const upButtons = screen.getAllByRole("button", { name: /profiles\.move_up/i });
    fireEvent.click(upButtons[1]);

    expect(modelInputs()[0]).toHaveValue("gpt-4o-mini");
    expect(modelInputs()[1]).toHaveValue("gpt-4");
  });

  it("Save calls useUpdateProfile().mutate with the expected slots", () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);

    fireEvent.click(screen.getAllByRole("button", { name: /profiles\.add_reserve/i })[0]);
    const modelInputs = () => screen.getAllByTestId(/^profile-model-text-/) as HTMLInputElement[];
    fireEvent.change(modelInputs()[1], { target: { value: "gpt-4o-mini" } });
    fireEvent.click(screen.getAllByRole("button", { name: /profiles\.move_up/i })[1]);

    fireEvent.click(screen.getByRole("button", { name: /^common\.save$/i }));

    expect(mockMutate).toHaveBeenCalledTimes(1);
    const [payload] = mockMutate.mock.calls[0];
    expect(payload.id).toBe("profile-1");
    expect(payload.name).toBe("Voice");
    expect(payload.slots.text).toEqual([
      { provider: "", model: "gpt-4o-mini" },
      { provider: "openai", model: "gpt-4" },
    ]);
  });

  it("changing the provider of a text row clears its model", async () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);

    const modelInput = screen.getByTestId("profile-model-text-0") as HTMLInputElement;
    expect(modelInput).toHaveValue("gpt-4");

    // ProviderSelect строки text — первый combobox в первой строке.
    // Выбираем ДРУГОЙ провайдер (не текущий "openai") — Radix не обязан
    // дёргать onValueChange при повторном выборе того же значения.
    const row = screen.getByTestId("profile-row-text-0");
    fireEvent.click(within(row).getAllByRole("combobox")[0]);
    fireEvent.click(await screen.findByRole("option", { name: /other-llm/ }));

    expect(screen.getByTestId("profile-model-text-0")).toHaveValue("");
  });

  it("model field is disabled until a provider is chosen", () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: /profiles\.add_reserve/i })[0]);
    // новая строка: provider = "" → model input задизейблен
    expect(screen.getByTestId("profile-model-text-1")).toBeDisabled();
  });
});
