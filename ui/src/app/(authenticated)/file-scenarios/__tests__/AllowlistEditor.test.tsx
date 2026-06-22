import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import { AllowlistEditor } from "../AllowlistEditor";

describe("AllowlistEditor", () => {
  it("renders exactly the closed-domain members", () => {
    render(<AllowlistEditor rows={[]} onToggle={() => {}} />);
    for (const name of ["transcribe", "describe", "extract_document", "save"]) {
      expect(screen.getByLabelText(name)).toBeInTheDocument();
    }
    // No free-text add input exists (closed domain).
    expect(screen.queryByPlaceholderText(/add/i)).toBeNull();
  });

  it("reflects enabled state and emits on toggle", () => {
    const onToggle = vi.fn();
    render(
      <AllowlistEditor
        rows={[{ action_ref: "describe", enabled: false }]}
        onToggle={onToggle}
      />,
    );
    const sw = screen.getByLabelText("describe");
    expect(sw).not.toBeChecked();
    fireEvent.click(sw);
    expect(onToggle).toHaveBeenCalledWith("describe", true);
  });
});
