import { describe, it, expect } from "vitest";
import en from "../locales/en.json";
import ru from "../locales/ru.json";

/**
 * Regression guard for the `{chars}` interpolation bug: use-translation.ts's
 * `t()` only replaces `{{key}}` (double braces). Any locale string written
 * with single braces (`{key}`) leaks the raw placeholder to the user instead
 * of being interpolated (e.g. "Show {chars}K more..." instead of "Show 12K
 * more...").
 *
 * A `{` counts as "single-brace" only when it is not immediately adjacent to
 * another `{` or `}` — i.e. it is not part of a `{{...}}` pair.
 */
const SINGLE_BRACE_PLACEHOLDER = /(?<![{}])\{[a-zA-Z_][a-zA-Z0-9_]*\}(?!\})/;

function findSingleBraceViolations(locale: Record<string, string>): string[] {
  return Object.entries(locale)
    .filter(([, value]) => SINGLE_BRACE_PLACEHOLDER.test(value))
    .map(([key, value]) => `${key}: "${value}"`);
}

describe("locale files: no single-brace placeholders", () => {
  it("en.json has no {word} placeholder outside {{word}}", () => {
    const violations = findSingleBraceViolations(en);
    expect(violations, `Found single-brace placeholders in en.json:\n${violations.join("\n")}`).toEqual([]);
  });

  it("ru.json has no {word} placeholder outside {{word}}", () => {
    const violations = findSingleBraceViolations(ru);
    expect(violations, `Found single-brace placeholders in ru.json:\n${violations.join("\n")}`).toEqual([]);
  });
});
