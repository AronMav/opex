import "@testing-library/jest-dom/vitest";
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { ProviderSortableGroup } from "../ProviderSortableGroup";
import type { Provider } from "@/types/api";

const mk = (name: string): Provider =>
  ({ id: name, name, type: "stt", provider_type: "whisper", enabled: true } as Provider);

const noop = () => {};

describe("ProviderSortableGroup", () => {
  it("renders one draggable row per active provider", () => {
    render(
      <ProviderSortableGroup
        cap="stt"
        activeProviders={[mk("a"), mk("b")]}
        typeLabelFor={(p) => p.provider_type}
        onReorder={noop}
        onToggleActive={noop}
        onEdit={noop}
        onDelete={noop}
      />,
    );
    expect(screen.getByText("a")).toBeInTheDocument();
    expect(screen.getByText("b")).toBeInTheDocument();
    // each active row exposes a drag handle (aria-label from providers.drag_handle_aria)
    expect(screen.getAllByRole("button", { name: /reorder|порядк/i }).length).toBeGreaterThanOrEqual(2);
  });
});
