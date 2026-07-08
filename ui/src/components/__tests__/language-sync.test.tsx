import { describe, it, expect, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { LanguageSync } from "@/components/language-sync";
import { useLanguageStore } from "@/stores/language-store";

// M6 (static-export variant): the app is `output: "export"`, so <html lang> can
// only be corrected client-side. LanguageSync mirrors the persisted locale onto
// document.documentElement.lang after hydration.
describe("LanguageSync (M6)", () => {
  afterEach(() => {
    cleanup();
    useLanguageStore.setState({ locale: "ru" });
  });

  it("reflects the store locale on document.documentElement.lang", () => {
    useLanguageStore.setState({ locale: "en" });
    render(<LanguageSync />);
    expect(document.documentElement.lang).toBe("en");

    useLanguageStore.setState({ locale: "ru" });
    render(<LanguageSync />);
    expect(document.documentElement.lang).toBe("ru");
  });
});
