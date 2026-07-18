import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

const { apiGet } = vi.hoisted(() => ({ apiGet: vi.fn() }));
vi.mock("@/lib/api", () => ({ apiGet }));

import { ModelCombobox } from "../ModelCombobox";

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

// Stateful harness for behaviours that depend on `value` updating after
// onChange (the component is CONTROLLED — it filters by its `value` prop, so a
// static vi.fn() mock would leave value="" and the filter would never engage).
function Controlled({ providerId, initial = "" }: { providerId?: string | null; initial?: string }) {
  const [v, setV] = React.useState(initial);
  return <ModelCombobox value={v} onChange={setV} providerId={providerId} data-testid="cb" />;
}

describe("ModelCombobox", () => {
  beforeEach(() => {
    apiGet.mockReset();
  });

  it("does not fetch until opened, then lazily loads models for providerId", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2", context_window: 200000 }, { id: "glm-5-air" }] });
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);

    expect(apiGet).not.toHaveBeenCalled();

    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByRole("option", { name: /glm-5\.2/ })).toBeInTheDocument();
    expect(apiGet).toHaveBeenCalledWith("/api/providers/p1/models");
  });

  it("clicking an option calls onChange with the model id and closes the list", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }, { id: "glm-5-air" }] });
    const onChange = vi.fn();
    wrap(<ModelCombobox value="" onChange={onChange} providerId="p1" data-testid="cb" />);

    fireEvent.focus(screen.getByTestId("cb"));
    fireEvent.mouseDown(await screen.findByRole("option", { name: /glm-5-air/ }));

    expect(onChange).toHaveBeenCalledWith("glm-5-air");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("typing filters the list case-insensitively", async () => {
    // Controlled harness: value must update after onChange for the filter (which
    // reads `value`) to engage — a static mock would leave value="".
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }, { id: "MiniMax-M2.5" }] });
    wrap(<Controlled providerId="p1" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    await screen.findByRole("option", { name: /glm-5\.2/ });
    fireEvent.change(input, { target: { value: "minimax" } });

    expect(input).toHaveValue("minimax"); // free text is legal
    expect(screen.getAllByRole("option")).toHaveLength(1);
    expect(screen.getByRole("option", { name: /MiniMax-M2\.5/ })).toBeInTheDocument();
  });

  it("reopening after selecting a value shows the full list (filter only after typing)", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }, { id: "MiniMax-M2.5" }] });
    wrap(<Controlled providerId="p1" initial="glm-5.2" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    // value is "glm-5.2" but filterActive is false on fresh open → both options show
    expect(await screen.findByRole("option", { name: /MiniMax-M2\.5/ })).toBeInTheDocument();
    expect(screen.getAllByRole("option")).toHaveLength(2);
  });

  it("typing after Escape-close keeps the filter engaged on reopen (filter-state race)", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }, { id: "MiniMax-M2.5" }] });
    wrap(<Controlled providerId="p1" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    await screen.findByRole("option", { name: /glm-5\.2/ });

    fireEvent.change(input, { target: { value: "glm" } });
    expect(screen.getAllByRole("option")).toHaveLength(1);

    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    fireEvent.change(input, { target: { value: "glmx" } });

    expect(screen.getByText("fields.model_no_match")).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /MiniMax-M2\.5/ })).not.toBeInTheDocument();
  });

  it("value not present in the list is allowed (free text, no error UI)", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }] });
    wrap(<ModelCombobox value="custom/model-id" onChange={vi.fn()} providerId="p1" data-testid="cb" />);
    expect(screen.getByTestId("cb")).toHaveValue("custom/model-id");
  });

  it("empty discovery result shows the 'list unavailable' hint row", async () => {
    apiGet.mockResolvedValue({ models: [] });
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByText("fields.model_list_unavailable")).toBeInTheDocument();
  });

  it("fetch error degrades to the same hint row (no toast, no crash)", async () => {
    apiGet.mockRejectedValue(new Error("boom"));
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByText("fields.model_list_unavailable")).toBeInTheDocument();
  });

  it("staticOptions mode renders without any network call", async () => {
    wrap(<ModelCombobox value="" onChange={vi.fn()} staticOptions={["gpt-4.1", "o3"]} data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByRole("option", { name: /gpt-4\.1/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /^o3$/ })).toBeInTheDocument();
    expect(apiGet).not.toHaveBeenCalled();
  });

  it("disabled input does not open the list", () => {
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" disabled data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(apiGet).not.toHaveBeenCalled();
  });

  it("keyboard: ArrowDown + Enter selects the highlighted option", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "a-model" }, { id: "b-model" }] });
    const onChange = vi.fn();
    wrap(<ModelCombobox value="" onChange={onChange} providerId="p1" data-testid="cb" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    await screen.findByRole("option", { name: /a-model/ });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("b-model");
  });

  it("aria-activedescendant points at the keyboard-highlighted option", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "a-model" }, { id: "b-model" }] });
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    await screen.findByRole("option", { name: /a-model/ });
    // Nothing highlighted yet → no active descendant.
    expect(input).not.toHaveAttribute("aria-activedescendant");

    fireEvent.keyDown(input, { key: "ArrowDown" });
    const active = input.getAttribute("aria-activedescendant");
    expect(active).toBeTruthy();
    // The referenced id must be a real option element in the listbox.
    const highlighted = document.getElementById(active as string);
    expect(highlighted).toHaveAttribute("role", "option");
    expect(highlighted).toHaveTextContent("a-model");
  });

  it("ArrowUp from the initial state does not highlight the first option", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "a-model" }, { id: "b-model" }] });
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    await screen.findByRole("option", { name: /a-model/ });
    fireEvent.keyDown(input, { key: "ArrowUp" });
    // No jump to index 0 — still nothing highlighted.
    expect(input).not.toHaveAttribute("aria-activedescendant");
  });
});
