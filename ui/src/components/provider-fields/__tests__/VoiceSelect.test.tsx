import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

const { apiGet } = vi.hoisted(() => ({ apiGet: vi.fn() }));
vi.mock("@/lib/api", () => ({ apiGet }));

import { VoiceSelect } from "../VoiceSelect";

window.HTMLElement.prototype.scrollIntoView = vi.fn();
window.HTMLElement.prototype.hasPointerCapture = vi.fn();
window.HTMLElement.prototype.releasePointerCapture = vi.fn();

// Открытие триггера через fireEvent.click, а не .pointerDown — см. ProviderSelect.test.tsx:
// в этом jsdom-окружении Radix Select открывается по click, не по synthetic pointerDown.
//
// apiGet.mockReset() живёт в afterEach, а не beforeEach: VoiceSelect запускает
// запрос СРАЗУ при монтировании (enabled с первого рендера — иначе нельзя решить
// Select/Input до рендера). Reset в beforeEach непосредственно перед этим eager-
// fetch'ем создаёт гонку с отслеживанием unhandled rejection в этом окружении
// (voспроизведено изолированно: тот же тест с beforeEach(() => apiGet.mockReset())
// падает детерминированно на "toolgate down"; перенос в afterEach убирает гонку
// без изменения семантики — мок всё равно чист к началу каждого теста).

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

describe("VoiceSelect", () => {
  afterEach(() => apiGet.mockReset());

  it("renders voices from /api/tts/voices for the provider", async () => {
    apiGet.mockResolvedValue({ voices: [{ id: "clone:Arty", name: "Arty" }, { id: "nova", name: "Nova", language: "en" }] });
    wrap(<VoiceSelect value="" onChange={vi.fn()} providerName="minimax" />);

    const trigger = await screen.findByRole("combobox");
    fireEvent.click(trigger);
    expect(await screen.findByRole("option", { name: /Arty/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /Nova/ })).toBeInTheDocument();
    expect(apiGet).toHaveBeenCalledWith("/api/tts/voices?provider=minimax");
  });

  it("selecting a voice calls onChange with its id", async () => {
    apiGet.mockResolvedValue({ voices: [{ id: "clone:Arty", name: "Arty" }] });
    const onChange = vi.fn();
    wrap(<VoiceSelect value="" onChange={onChange} providerName="minimax" />);
    fireEvent.click(await screen.findByRole("combobox"));
    fireEvent.click(await screen.findByRole("option", { name: /Arty/ }));
    expect(onChange).toHaveBeenCalledWith("clone:Arty");
  });

  it("allowServerDefault adds the dash item mapping to empty string", async () => {
    apiGet.mockResolvedValue({ voices: [{ id: "nova", name: "Nova" }] });
    const onChange = vi.fn();
    wrap(<VoiceSelect value="nova" onChange={onChange} providerName="minimax" allowServerDefault />);
    fireEvent.click(await screen.findByRole("combobox"));
    fireEvent.click(await screen.findByRole("option", { name: /voice_server_default/ }));
    expect(onChange).toHaveBeenCalledWith("");
  });

  it("empty voice list degrades to a free-text input", async () => {
    apiGet.mockResolvedValue({ voices: [] });
    const onChange = vi.fn();
    wrap(<VoiceSelect value="" onChange={onChange} providerName="broken-tts" />);
    const input = await screen.findByRole("textbox");
    fireEvent.change(input, { target: { value: "custom-voice" } });
    expect(onChange).toHaveBeenCalledWith("custom-voice");
  });

  it("fetch error degrades to a free-text input", async () => {
    apiGet.mockRejectedValue(new Error("toolgate down"));
    wrap(<VoiceSelect value="v1" onChange={vi.fn()} providerName="broken-tts" />);
    expect(await screen.findByRole("textbox")).toHaveValue("v1");
  });

  it("no provider → disabled, no fetch", () => {
    wrap(<VoiceSelect value="" onChange={vi.fn()} providerName="" />);
    expect(apiGet).not.toHaveBeenCalled();
  });
});
