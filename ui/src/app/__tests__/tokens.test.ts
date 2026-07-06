import { test, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const css = readFileSync(
  resolve(__dirname, "../globals.css"),
  "utf8",
);

test.each([
  "--text-2xs",
  "--text-3xs",
  "--text-code",
  "--text-message",
  "--sidebar-w",
  "--toolbar-h",
  "--explorer-w",
  "--auth-w",
  "--z-modal",
  "--z-popover",
  "--elevation-1",
  "--tap-target",
])("globals.css declares %s", (token) => {
  expect(css).toContain(token);
});

test("exposes shadow-elev + tap-target utilities", () => {
  expect(css).toMatch(/@utility\s+shadow-elev-1/);
  expect(css).toMatch(/@utility\s+tap-target/);
});
