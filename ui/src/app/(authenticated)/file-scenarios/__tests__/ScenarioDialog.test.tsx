import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import { ScenarioDialog } from "../ScenarioDialog";
import type { CreateFileScenarioInput, UpdateFileScenarioInput } from "@/types/api";

const toolForm: CreateFileScenarioInput = {
  match_type: "image/*", executor: "tool", action_ref: "code_exec",
  label: "Bad", is_default: true, priority: 100, enabled: true,
};

const skillForm: CreateFileScenarioInput = {
  match_type: "image/*", executor: "skill", action_ref: "ocr_skill",
  label: "Skill binding", is_default: false, priority: 50, enabled: true,
};

const editableForm: CreateFileScenarioInput = {
  match_type: "application/pdf", executor: "tool", action_ref: "extract_document",
  label: "PDF extractor", is_default: false, priority: 100, enabled: true,
};

describe("ScenarioDialog — allowlist default guard", () => {
  it("disables the default switch for a non-allowlisted tool action", () => {
    render(
      <ScenarioDialog open editing={false} form={toolForm} setForm={() => {}} saving={false} onSave={() => {}} onClose={() => {}} />,
    );
    const sw = screen.getByLabelText("file_scenarios.is_default");
    expect(sw).toBeDisabled();
    expect(screen.getByText("file_scenarios.default_not_allowlisted")).toBeInTheDocument();
  });

  it("enables the default switch for an allowlisted tool action", () => {
    render(
      <ScenarioDialog open editing={false} form={{ ...toolForm, action_ref: "describe" }} setForm={() => {}} saving={false} onSave={() => {}} onClose={() => {}} />,
    );
    expect(screen.getByLabelText("file_scenarios.is_default")).not.toBeDisabled();
  });
});

describe("ScenarioDialog — skill default-guard (Fix A)", () => {
  it("disables the default switch and shows skill warning when executor=skill", () => {
    render(
      <ScenarioDialog open editing={false} form={skillForm} setForm={() => {}} saving={false} onSave={() => {}} onClose={() => {}} />,
    );
    const sw = screen.getByLabelText("file_scenarios.is_default");
    expect(sw).toBeDisabled();
    expect(screen.getByText("file_scenarios.default_not_skill")).toBeInTheDocument();
  });
});

describe("ScenarioDialog — edit-mode field-locking (Fix C)", () => {
  it("renders match_type, action_ref, and executor as disabled inputs in edit mode", () => {
    render(
      <ScenarioDialog open editing form={editableForm} setForm={() => {}} saving={false} onSave={() => {}} onClose={() => {}} />,
    );
    // match_type is a disabled Input
    const matchInput = screen.getByDisplayValue("application/pdf");
    expect(matchInput).toBeDisabled();

    // action_ref is a disabled Input
    const actionInput = screen.getByDisplayValue("extract_document");
    expect(actionInput).toBeDisabled();

    // executor is a disabled Input (not a Select) in edit mode
    const executorInput = screen.getByDisplayValue("tool");
    expect(executorInput).toBeDisabled();

    // There should be no combobox (Select) for executor in edit mode
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });
});

describe("ScenarioDialog — edit-mode payload shape (Fix B)", () => {
  it("emits only {label,priority,enabled} on save in edit mode", () => {
    const onSave = vi.fn();
    render(
      <ScenarioDialog open editing form={editableForm} setForm={() => {}} saving={false} onSave={onSave} onClose={() => {}} />,
    );
    const saveBtn = screen.getByText("common.save");
    fireEvent.click(saveBtn);

    expect(onSave).toHaveBeenCalledOnce();
    const payload = onSave.mock.calls[0][0] as UpdateFileScenarioInput;

    // Must contain the mutable fields
    expect(payload).toHaveProperty("label", "PDF extractor");
    expect(payload).toHaveProperty("priority", 100);
    expect(payload).toHaveProperty("enabled", true);

    // Must NOT contain structural fields
    expect(payload).not.toHaveProperty("match_type");
    expect(payload).not.toHaveProperty("executor");
    expect(payload).not.toHaveProperty("action_ref");
    expect(payload).not.toHaveProperty("is_default");
  });
});
