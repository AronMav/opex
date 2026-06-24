import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { ProviderRow } from "../ProviderRow";
import type { Provider } from "@/types/api";

const mk = (over: Partial<Provider> = {}): Provider =>
  ({ id: "p1", name: "whisper", type: "stt", provider_type: "whisper", enabled: true, ...over } as Provider);

const noop = () => {};

describe("ProviderRow", () => {
  it("renders provider name", () => {
    render(
      <ProviderRow
        provider={mk()} cap="stt" isActive={false} typeLabel="whisper-model"
        isCapabilityGroup onToggleActive={noop} onEdit={noop} onDelete={noop}
      />,
    );
    expect(screen.getByText("whisper")).toBeInTheDocument();
  });

  it("renders an active-toggle switch for capability rows", () => {
    render(
      <ProviderRow
        provider={mk()} cap="stt" isActive onToggleActive={noop} typeLabel="whisper"
        isCapabilityGroup onEdit={noop} onDelete={noop}
      />,
    );
    expect(screen.getAllByRole("switch")).toHaveLength(1);
  });

  it("does NOT render a switch for non-capability (text) rows", () => {
    render(
      <ProviderRow
        provider={mk({ type: "text", provider_type: "openai" })} cap="text" isActive={false}
        typeLabel="OpenAI" isCapabilityGroup={false} onToggleActive={noop} onEdit={noop} onDelete={noop}
      />,
    );
    expect(screen.queryAllByRole("switch")).toHaveLength(0);
  });
});
