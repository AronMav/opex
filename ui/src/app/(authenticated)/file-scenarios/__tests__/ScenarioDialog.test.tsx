import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import { ScenarioDialog } from "../ScenarioDialog";
import type { CreateFileScenarioInput } from "@/types/api";

const form: CreateFileScenarioInput = {
  match_type: "image/*", executor: "tool", action_ref: "code_exec",
  label: "Bad", is_default: true, priority: 100, enabled: true,
};

describe("ScenarioDialog", () => {
  it("disables the default switch for a non-allowlisted tool action", () => {
    render(
      <ScenarioDialog open editing={false} form={form} setForm={() => {}} saving={false} onSave={() => {}} onClose={() => {}} />,
    );
    const sw = screen.getByLabelText("file_scenarios.is_default");
    expect(sw).toBeDisabled();
    expect(screen.getByText("file_scenarios.default_not_allowlisted")).toBeInTheDocument();
  });

  it("enables the default switch for an allowlisted tool action", () => {
    render(
      <ScenarioDialog open editing={false} form={{ ...form, action_ref: "describe" }} setForm={() => {}} saving={false} onSave={() => {}} onClose={() => {}} />,
    );
    expect(screen.getByLabelText("file_scenarios.is_default")).not.toBeDisabled();
  });
});
