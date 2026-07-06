import { test, expect } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { resolve, join } from "node:path";

const UI_SRC = resolve(__dirname, "..", "..", "src");

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      walk(full, out);
    } else if (/\.(ts|tsx)$/.test(full) && !/\.test\.(ts|tsx)$/.test(full)) {
      out.push(full);
    }
  }
  return out;
}

const files = walk(UI_SRC);

// Regression guard: the font-size token scale (text-3xs / text-2xs / text-code /
// text-message + the Tailwind scale) is established in globals.css. No source
// file should reintroduce an arbitrary `text-[Npx]` / `text-[Nrem]` value.
// This complements the ESLint `no-raw-font-sizes` rule with a hard CI gate so
// the invariant survives even if lint config is loosened.
const RAW_FONT_RE = /text-\[\d+(?:\.\d+)?(?:px|rem)\]/;

test("no source file uses arbitrary font-size values", () => {
  expect(files.length).toBeGreaterThan(0);
  const offenders: string[] = [];
  for (const f of files) {
    const src = readFileSync(f, "utf8");
    const m = src.match(RAW_FONT_RE);
    if (m) offenders.push(`${f}: "${m[0]}"`);
  }
  expect(
    offenders,
    `raw font sizes found:\n${offenders.join("\n")}`,
  ).toEqual([]);
});
