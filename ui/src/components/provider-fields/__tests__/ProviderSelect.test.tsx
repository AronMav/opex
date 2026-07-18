import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

vi.mock("@/lib/queries", () => ({
  useProviders: () => ({
    data: [
      { id: "p1", name: "my-openai", type: "text", provider_type: "openai_compat", default_model: "gpt-4.1", enabled: true },
      { id: "p2", name: "legacy-llm", type: "llm", provider_type: "openai_compat", default_model: "old-model", enabled: true },
      { id: "p3", name: "whisper", type: "stt", provider_type: "openai-compatible", default_model: null, enabled: true },
    ],
  }),
}));

import { ProviderSelect } from "../ProviderSelect";

// jsdom не реализует scrollIntoView/pointer capture, которые дергает Radix Select.
window.HTMLElement.prototype.scrollIntoView = vi.fn();
window.HTMLElement.prototype.hasPointerCapture = vi.fn();
window.HTMLElement.prototype.releasePointerCapture = vi.fn();

// Открытие триггера через fireEvent.click, а не .pointerDown: Radix's onPointerDown
// only opens the popup when event.pointerType === "mouse", and RTL's synthetic
// pointerDown defaults pointerType to "". onClick opens whenever the tracked
// pointer type ref isn't "mouse" (its initial value is "touch"), which is what
// a plain fireEvent.click produces here — so click is the reliable jsdom trigger.

describe("ProviderSelect", () => {
  it("offers only providers whose type is in `categories` (text+llm pair)", () => {
    render(<ProviderSelect value="" onChange={vi.fn()} categories={["text", "llm"]} />);
    fireEvent.click(screen.getByRole("combobox"));
    expect(screen.getByRole("option", { name: /my-openai/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /legacy-llm/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /whisper/ })).not.toBeInTheDocument();
  });

  it("category filter for a media capability", () => {
    render(<ProviderSelect value="" onChange={vi.fn()} categories={["stt"]} />);
    fireEvent.click(screen.getByRole("combobox"));
    expect(screen.getByRole("option", { name: /whisper/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /my-openai/ })).not.toBeInTheDocument();
  });

  it("shows the provider's default_model as a secondary label", () => {
    render(<ProviderSelect value="" onChange={vi.fn()} categories={["text"]} />);
    fireEvent.click(screen.getByRole("combobox"));
    expect(screen.getByRole("option", { name: /my-openai/ })).toHaveTextContent("gpt-4.1");
  });

  it("allowNone renders the dash item and maps it to empty string", () => {
    const onChange = vi.fn();
    render(<ProviderSelect value="my-openai" onChange={onChange} categories={["text", "llm"]} allowNone />);
    fireEvent.click(screen.getByRole("combobox"));
    fireEvent.click(screen.getByRole("option", { name: "—" }));
    expect(onChange).toHaveBeenCalledWith("");
  });

  it("selecting a provider calls onChange with its name", () => {
    const onChange = vi.fn();
    render(<ProviderSelect value="" onChange={onChange} categories={["text", "llm"]} />);
    fireEvent.click(screen.getByRole("combobox"));
    fireEvent.click(screen.getByRole("option", { name: /legacy-llm/ }));
    expect(onChange).toHaveBeenCalledWith("legacy-llm");
  });

  it("a stale value not in the options still shows in the trigger (no blank field)", () => {
    // "ghost-provider" isn't in the mocked provider list — the trigger must not
    // go blank; it surfaces the configured value so the user sees what's set,
    // and must not fire a spurious onChange just by rendering.
    const onChange = vi.fn();
    render(<ProviderSelect value="ghost-provider" onChange={onChange} categories={["text", "llm"]} />);
    expect(screen.getByRole("combobox")).toHaveTextContent("ghost-provider");
    expect(onChange).not.toHaveBeenCalled();
  });

  it("forwards data-testid to the trigger", () => {
    render(<ProviderSelect value="" onChange={vi.fn()} categories={["text"]} data-testid="prov" />);
    expect(screen.getByTestId("prov")).toBeInTheDocument();
  });
});
