import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import { ScenarioRow } from "../ScenarioRow";
import type { FileScenario } from "@/types/api";

const base: FileScenario = {
  id: "s1", match_type: "image/*", executor: "tool", action_ref: "describe",
  label: "Describe image", is_default: false, priority: 100, enabled: true,
  scope: "global", created_by: "system",
  created_at: "2026-06-22T00:00:00Z", updated_at: "2026-06-22T00:00:00Z",
};

describe("ScenarioRow", () => {
  it("renders label and executor", () => {
    render(
      <ScenarioRow scenario={base} onToggleDefault={() => {}} onToggleEnabled={() => {}} onEdit={() => {}} onDelete={() => {}} />,
    );
    expect(screen.getByText("Describe image")).toBeInTheDocument();
    expect(screen.getByText("tool")).toBeInTheDocument();
  });

  it("calls onToggleDefault when default control is clicked", () => {
    const onToggleDefault = vi.fn();
    render(
      <ScenarioRow scenario={base} onToggleDefault={onToggleDefault} onToggleEnabled={() => {}} onEdit={() => {}} onDelete={() => {}} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /file_scenarios.set_default/i }));
    expect(onToggleDefault).toHaveBeenCalledTimes(1);
  });

  it("shows default badge when is_default is true", () => {
    render(
      <ScenarioRow scenario={{ ...base, is_default: true }} onToggleDefault={() => {}} onToggleEnabled={() => {}} onEdit={() => {}} onDelete={() => {}} />,
    );
    expect(screen.getByText("file_scenarios.default_badge")).toBeInTheDocument();
  });

  it("shows disabled badge when enabled is false", () => {
    render(
      <ScenarioRow scenario={{ ...base, enabled: false }} onToggleDefault={() => {}} onToggleEnabled={() => {}} onEdit={() => {}} onDelete={() => {}} />,
    );
    expect(screen.getByText("file_scenarios.disabled_badge")).toBeInTheDocument();
  });

  it("does not show disabled badge when enabled is true", () => {
    render(
      <ScenarioRow scenario={{ ...base, enabled: true }} onToggleDefault={() => {}} onToggleEnabled={() => {}} onEdit={() => {}} onDelete={() => {}} />,
    );
    expect(screen.queryByText("file_scenarios.disabled_badge")).not.toBeInTheDocument();
  });

  it("calls onToggleEnabled with false when switch is toggled for an enabled row", () => {
    const onToggleEnabled = vi.fn();
    render(
      <ScenarioRow scenario={{ ...base, enabled: true }} onToggleDefault={() => {}} onToggleEnabled={onToggleEnabled} onEdit={() => {}} onDelete={() => {}} />,
    );
    fireEvent.click(screen.getByRole("switch", { name: /file_scenarios.toggle_enabled/i }));
    expect(onToggleEnabled).toHaveBeenCalledWith(false);
  });

  it("calls onEdit when edit button is clicked", () => {
    const onEdit = vi.fn();
    render(
      <ScenarioRow scenario={base} onToggleDefault={() => {}} onToggleEnabled={() => {}} onEdit={onEdit} onDelete={() => {}} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /common.edit/i }));
    expect(onEdit).toHaveBeenCalledTimes(1);
  });

  it("calls onDelete when delete button is clicked", () => {
    const onDelete = vi.fn();
    render(
      <ScenarioRow scenario={base} onToggleDefault={() => {}} onToggleEnabled={() => {}} onEdit={() => {}} onDelete={onDelete} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /common.delete/i }));
    expect(onDelete).toHaveBeenCalledTimes(1);
  });
});
