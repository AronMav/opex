import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import type { FileScenario, FileScenarioAllowlistRow } from "@/types/api";

const setDefaultMutate = vi.fn();
const updateMutate = vi.fn();
const updateMutateAsync = vi.fn().mockResolvedValue(undefined);
const createMutateAsync = vi.fn().mockResolvedValue(undefined);

// Mutable allowlist — tests can reassign before render to control the live set.
let mockAllowlist: FileScenarioAllowlistRow[] = [];

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
  useCreateFileScenario: () => ({ mutateAsync: createMutateAsync, isPending: false }),
  useUpdateFileScenario: () => ({ mutate: updateMutate, mutateAsync: updateMutateAsync }),
  useDeleteFileScenario: () => ({ mutate: vi.fn() }),
  useSetFileScenarioDefault: () => ({ mutate: setDefaultMutate }),
  useFileScenarioAllowlist: () => ({ data: mockAllowlist }),
  useSetFileScenarioAllowlist: () => ({ mutate: vi.fn() }),
}));

import FileScenariosPage from "../page";

// ── helper: extra scenarios for gate tests ────────────────────────────────────
/** A non-default skill scenario (executor="skill"). */
const skillScenario: FileScenario = {
  id: "skill-1", match_type: "application/pdf", executor: "skill",
  action_ref: "my_skill", label: "Skill scenario",
  is_default: false, priority: 50, enabled: true,
  scope: "global", created_by: "system",
  created_at: "2026-06-22T00:00:00Z", updated_at: "2026-06-22T00:00:00Z",
};

/** A non-default tool scenario with action_ref="describe". */
const toolDescribeScenario: FileScenario = {
  id: "tool-desc-1", match_type: "image/png", executor: "tool",
  action_ref: "describe", label: "Describe PNG",
  is_default: false, priority: 50, enabled: true,
  scope: "global", created_by: "system",
  created_at: "2026-06-22T00:00:00Z", updated_at: "2026-06-22T00:00:00Z",
};

/** A non-default tool scenario with action_ref="transcribe". */
const toolTranscribeScenario: FileScenario = {
  id: "tool-trans-1", match_type: "audio/mpeg", executor: "tool",
  action_ref: "transcribe", label: "Transcribe MP3",
  is_default: false, priority: 50, enabled: true,
  scope: "global", created_by: "system",
  created_at: "2026-06-22T00:00:00Z", updated_at: "2026-06-22T00:00:00Z",
};

/** Render the page with an overridden scenario list so we can target specific rows. */
function renderWithScenarios(extra: FileScenario[], allowlist: FileScenarioAllowlistRow[] = []) {
  // Temporarily replace the module-level list items.
  // The mock reads `scenarios` from closure; we splice in-place so the reference stays.
  const saved = scenarios.splice(0, scenarios.length, ...extra);
  mockAllowlist = allowlist;
  const result = render(<FileScenariosPage />);
  // Restore for other tests.
  scenarios.splice(0, scenarios.length, ...saved);
  return result;
}

describe("FileScenariosPage", () => {
  beforeEach(() => {
    setDefaultMutate.mockClear();
    updateMutate.mockClear();
    updateMutateAsync.mockClear();
    createMutateAsync.mockClear();
    mockAllowlist = [];
  });

  // ── Original tests ────────────────────────────────────────────────────────

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

  // ── I-1: LIVE allowlist set-default gate ──────────────────────────────────

  it("I-1a: skill scenario — set-default is blocked (mutate NOT called)", () => {
    // Render with only the skill scenario (is_default=false). Allowlist is empty.
    renderWithScenarios([skillScenario]);
    const stars = screen.getAllByRole("button", { name: /file_scenarios.set_default/i });
    // Clicking the star would try to set is_default=true → gate should reject.
    fireEvent.click(stars[0]);
    expect(setDefaultMutate).not.toHaveBeenCalled();
  });

  it("I-1b: tool 'describe' with empty allowlist — set-default is blocked (mutate NOT called)", () => {
    // Render with tool scenario (action_ref="describe", is_default=false).
    // mockAllowlist = [] → enabledAllowlistSet is empty → isAllowlistViolation returns true.
    renderWithScenarios([toolDescribeScenario], []);
    const stars = screen.getAllByRole("button", { name: /file_scenarios.set_default/i });
    fireEvent.click(stars[0]);
    expect(setDefaultMutate).not.toHaveBeenCalled();
  });

  it("I-1c: tool 'transcribe' with transcribe ENABLED — set-default IS called", () => {
    // Render with tool scenario (action_ref="transcribe", is_default=false).
    // Enable "transcribe" in the live allowlist → gate should pass.
    renderWithScenarios(
      [toolTranscribeScenario],
      [{ action_ref: "transcribe", enabled: true }],
    );
    const stars = screen.getAllByRole("button", { name: /file_scenarios.set_default/i });
    fireEvent.click(stars[0]);
    expect(setDefaultMutate).toHaveBeenCalledWith(
      expect.objectContaining({ id: "tool-trans-1", is_default: true }),
    );
  });

  // ── I-2: create vs edit onSave split ─────────────────────────────────────

  it("I-2a: create — opening dialog and submitting calls useCreateFileScenario.mutateAsync", async () => {
    render(<FileScenariosPage />);
    // Open create dialog
    const addBtn = screen.getByRole("button", { name: /file_scenarios.add/i });
    fireEvent.click(addBtn);
    // In create mode the dialog has 3 text inputs: match_type[0], action_ref[1], label[2].
    // The executor field is a <Select> (combobox) and priority is a spinbutton — neither
    // has role="textbox", so they are not returned by getAllByRole("textbox").
    const textboxes = screen.getAllByRole("textbox") as HTMLInputElement[];
    const matchInput = textboxes[0];
    const actionInput = textboxes[1];
    const labelInput = textboxes[2];
    fireEvent.change(matchInput, { target: { value: "image/*" } });
    fireEvent.change(actionInput, { target: { value: "describe" } });
    fireEvent.change(labelInput, { target: { value: "My new scenario" } });
    // Click create button
    const createBtn = screen.getByRole("button", { name: "common.create" });
    fireEvent.click(createBtn);
    // createMutateAsync should be called with full CreateFileScenarioInput body
    expect(createMutateAsync).toHaveBeenCalledWith(
      expect.objectContaining({
        match_type: "image/*",
        action_ref: "describe",
        label: "My new scenario",
        executor: "tool",
      }),
    );
    expect(updateMutateAsync).not.toHaveBeenCalled();
  });

  it("I-2b: edit — opening edit dialog and submitting calls useUpdateFileScenario.mutateAsync with ONLY label/priority/enabled", async () => {
    render(<FileScenariosPage />);
    // groupByMatchType sorts groups alphabetically, so audio/* group (aud-t) comes first.
    // editBtns[0] is therefore for aud-t (Transcribe), not img-d (Describe image).
    const editBtns = screen.getAllByRole("button", { name: /^common\.edit$/i });
    fireEvent.click(editBtns[0]);
    // In edit mode the dialog has 4 text inputs: match_type[0], executor[1], action_ref[2], label[3].
    // All structural fields are disabled <Input> elements (not a Select) in edit mode.
    const textboxes = screen.getAllByRole("textbox") as HTMLInputElement[];
    const labelInput = textboxes[3];
    fireEvent.change(labelInput, { target: { value: "Renamed label" } });
    // Click save
    const saveBtn = screen.getByRole("button", { name: "common.save" });
    fireEvent.click(saveBtn);
    // updateMutateAsync should be called with {id, label, priority, enabled} only.
    // aud-t is the first scenario in the sorted audio/* group.
    expect(updateMutateAsync).toHaveBeenCalledWith(
      expect.objectContaining({
        id: "aud-t",
        label: "Renamed label",
        priority: expect.any(Number),
        enabled: expect.any(Boolean),
      }),
    );
    // Structural fields must NOT be in the payload
    const call = updateMutateAsync.mock.calls[0][0] as Record<string, unknown>;
    expect(call).not.toHaveProperty("match_type");
    expect(call).not.toHaveProperty("executor");
    expect(call).not.toHaveProperty("action_ref");
    expect(call).not.toHaveProperty("is_default");
    expect(createMutateAsync).not.toHaveBeenCalled();
  });
});
