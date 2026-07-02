import { test, expect } from "vitest";
import { Linter } from "eslint";
import tsParser from "@typescript-eslint/parser";
import rule from "../no-raw-design-values.js";

function lint(code: string) {
  const linter = new Linter({ configType: "flat" });
  return linter.verify(code, {
    plugins: { local: { rules: { "no-raw-design-values": rule } } },
    languageOptions: {
      parser: tsParser,
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    rules: { "local/no-raw-design-values": "error" },
  });
}

test("flags arbitrary 10/11px font sizes", () => {
  const msgs = lint(`const x = <div className="text-[10px]" />;`);
  expect(msgs).toHaveLength(1);
});

test("flags raw neu-card usage", () => {
  const msgs = lint(`const x = <div className="neu-flat p-4" />;`);
  expect(msgs).toHaveLength(1);
});

test("flags raw palette colors", () => {
  const msgs = lint(`const x = <div className="bg-blue-500" />;`);
  expect(msgs).toHaveLength(1);
});

test("flags arbitrary px dimensions", () => {
  const msgs = lint(`const x = <div className="h-[600px]" />;`);
  expect(msgs).toHaveLength(1);
});

test("allows semantic utilities and vw/dvh", () => {
  const msgs = lint(
    `const x = <div className="bg-card text-muted-foreground max-w-[95vw] text-2xs" />;`,
  );
  expect(msgs).toHaveLength(0);
});
