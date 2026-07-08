import { describe, it, expect } from "vitest";

// ── Test getTranslations directly (avoids React hook context) ──────────────

import { getTranslations } from "@/i18n";

describe("getTranslations", () => {
  it("returns Russian translations for 'ru' locale", () => {
    const t = getTranslations("ru");
    expect(t).toBeDefined();
    expect(typeof t).toBe("object");
  });

  it("returns English translations for 'en' locale", () => {
    const t = getTranslations("en");
    expect(t).toBeDefined();
    expect(typeof t).toBe("object");
  });

  it("falls back to English for unknown locale", () => {
    const en = getTranslations("en");
    const fallback = getTranslations("fr" as any);
    expect(fallback).toBe(en);
  });

  it("translations have string values", () => {
    const t = getTranslations("en");
    const values = Object.values(t);
    expect(values.length).toBeGreaterThan(0);
    for (const v of values.slice(0, 10)) {
      expect(typeof v).toBe("string");
    }
  });
});

// ── Test real translation keys produce real output ──────────────────────────

describe("translation content", () => {
  it("ru and en have same keys", () => {
    const ru = getTranslations("ru");
    const en = getTranslations("en");
    const ruKeys = Object.keys(ru).sort();
    const enKeys = Object.keys(en).sort();
    expect(ruKeys).toEqual(enKeys);
  });

  it("common.save exists and differs between locales", () => {
    const ru = getTranslations("ru");
    const en = getTranslations("en");
    expect(ru["common.save"]).toBeDefined();
    expect(en["common.save"]).toBeDefined();
    expect(ru["common.save"]).not.toBe(en["common.save"]);
  });

  it("keys with {{}} placeholders contain placeholder markers", () => {
    const t = getTranslations("en");
    const withPlaceholders = Object.entries(t).filter(([, v]) => v.includes("{{"));
    expect(withPlaceholders.length).toBeGreaterThan(0);
    for (const [key, val] of withPlaceholders) {
      // Every {{ must have a matching }}
      const opens = (val.match(/\{\{/g) || []).length;
      const closes = (val.match(/\}\}/g) || []).length;
      expect(opens).toBe(closes);
    }
  });

  it("no empty translation values", () => {
    const t = getTranslations("ru");
    const empty = Object.entries(t).filter(([, v]) => v.trim() === "");
    expect(empty).toEqual([]);
  });
});

// ── Test language store ─────────────────────────────────────────────────────

import { useLanguageStore, LOCALES } from "@/stores/language-store";
import type { Locale } from "@/stores/language-store";

describe("language store", () => {
  it("has default locale", () => {
    const locale = useLanguageStore.getState().locale;
    expect(["ru", "en"]).toContain(locale);
  });

  it("setLocale changes locale", () => {
    useLanguageStore.getState().setLocale("en");
    expect(useLanguageStore.getState().locale).toBe("en");

    useLanguageStore.getState().setLocale("ru");
    expect(useLanguageStore.getState().locale).toBe("ru");
  });
});

describe("LOCALES constant", () => {
  it("contains ru and en", () => {
    const values = LOCALES.map((l) => l.value);
    expect(values).toContain("ru");
    expect(values).toContain("en");
  });

  it("each locale has a label", () => {
    for (const loc of LOCALES) {
      expect(typeof loc.label).toBe("string");
      expect(loc.label.length).toBeGreaterThan(0);
    }
  });
});
