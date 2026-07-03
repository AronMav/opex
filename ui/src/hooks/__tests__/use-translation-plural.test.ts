import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useTranslation } from "../use-translation";
import { useLanguageStore } from "@/stores/language-store";

/**
 * Pluralization via Intl.PluralRules (B3).
 *
 * `t("key", { count })` prefers the CLDR plural-variant key
 * (`<key>_one` / `_few` / `_many` / `_other`) selected by
 * `new Intl.PluralRules(locale).select(count)`, falling back to the base
 * `<key>` when no variant exists. This must stay 100% backward-compatible:
 * keys without `_one`/`_few`/… variants resolve exactly as before.
 */

function setLocale(locale: "en" | "ru") {
  act(() => {
    useLanguageStore.getState().setLocale(locale);
  });
}

describe("useTranslation — pluralization", () => {
  beforeEach(() => setLocale("en"));
  afterEach(() => setLocale("ru"));

  it("selects English plural categories (1 → one, 2 → other)", () => {
    setLocale("en");
    const { result } = renderHook(() => useTranslation());
    // chat.sessions_count_one = "{{count}} session", _other = "{{count}} sessions"
    expect(result.current.t("chat.sessions_count", { count: 1 })).toBe("1 session");
    expect(result.current.t("chat.sessions_count", { count: 2 })).toBe("2 sessions");
    expect(result.current.t("chat.sessions_count", { count: 0 })).toBe("0 sessions");
  });

  it("selects Russian plural categories (1 → one, 2 → few, 5 → many)", () => {
    setLocale("ru");
    const { result } = renderHook(() => useTranslation());
    // one=сессия, few=сессии, many=сессий
    expect(result.current.t("chat.sessions_count", { count: 1 })).toBe("1 сессия");
    expect(result.current.t("chat.sessions_count", { count: 2 })).toBe("2 сессии");
    expect(result.current.t("chat.sessions_count", { count: 5 })).toBe("5 сессий");
    expect(result.current.t("chat.sessions_count", { count: 21 })).toBe("21 сессия");
  });

  it("pluralizes routing-rules count (ru: 1→one, 3→few, 11→many)", () => {
    setLocale("ru");
    const { result } = renderHook(() => useTranslation());
    expect(result.current.t("agents.routing_rules_count", { count: 1 })).toBe("1 правило");
    expect(result.current.t("agents.routing_rules_count", { count: 3 })).toBe("3 правила");
    expect(result.current.t("agents.routing_rules_count", { count: 11 })).toBe("11 правил");
  });

  it("is backward-compatible: keys with no plural variants resolve to the base key", () => {
    setLocale("en");
    const { result } = renderHook(() => useTranslation());
    // chat.delete_all_confirm_description has {{count}} but NO _one/_few variants —
    // must interpolate against the single base string, exactly as before.
    const out = result.current.t("chat.delete_all_confirm_description", {
      count: 1,
      agent: "Bob",
    });
    expect(out).toContain("All 1 sessions of agent Bob");
    expect(out).not.toContain("{{count}}");
  });

  it("ignores plural logic entirely when no count is passed", () => {
    setLocale("en");
    const { result } = renderHook(() => useTranslation());
    // No count → base key, no accidental variant lookup.
    expect(result.current.t("checkpoints.title")).toBe("Checkpoints");
  });
});
