import { test, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const css = readFileSync(
  resolve(__dirname, "../globals.css"),
  "utf8",
);

// First (:root / light) hex value of a token.
function lightToken(name: string): string {
  const m = css.match(new RegExp(name + String.raw`:\s*(#[0-9a-fA-F]{6})`));
  if (!m) throw new Error(`light token ${name} not found`);
  return m[1];
}
// Hex value of a token inside the .dark { … } block.
function darkToken(name: string): string {
  const dark = css.slice(css.indexOf(".dark {"));
  const m = dark.match(new RegExp(name + String.raw`:\s*(#[0-9a-fA-F]{6})`));
  if (!m) throw new Error(`dark token ${name} not found`);
  return m[1];
}

// WCAG relative luminance + contrast ratio for #rrggbb pairs.
function lum(hex: string): number {
  const n = hex.replace("#", "");
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(n.slice(i, i + 2), 16) / 255);
  const f = (c: number) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}
function ratio(a: string, b: string): number {
  const [x, y] = [lum(a), lum(b)].sort((m, n) => n - m);
  return (x + 0.05) / (y + 0.05);
}

const LIGHT_CARD = "#eaeff7";
const DARK_PRIMARY = "#6b9eff";

test("light --success is AA on --card", () => {
  expect(ratio(lightToken("--success"), LIGHT_CARD)).toBeGreaterThanOrEqual(4.5);
});
test("light --warning is AA on --card", () => {
  expect(ratio(lightToken("--warning"), LIGHT_CARD)).toBeGreaterThanOrEqual(4.5);
});
test("light --muted-foreground-subtle is AA on --card", () => {
  expect(
    ratio(lightToken("--muted-foreground-subtle"), LIGHT_CARD),
  ).toBeGreaterThanOrEqual(4.5);
});
test("light --muted-foreground is enhanced-AA on --card", () => {
  // Muted foreground carries informative secondary text (timestamps, token
  // counts, hints) — hold it to a stricter 5.5:1 so it stays legible on the
  // lightest surface (--card) even before per-class alpha is applied.
  expect(
    ratio(lightToken("--muted-foreground"), LIGHT_CARD),
  ).toBeGreaterThanOrEqual(5.5);
});
test("dark --primary-foreground is AA on --primary", () => {
  expect(ratio(darkToken("--primary-foreground"), DARK_PRIMARY)).toBeGreaterThanOrEqual(4.5);
});
