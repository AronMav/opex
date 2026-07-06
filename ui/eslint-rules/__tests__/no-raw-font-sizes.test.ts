import { test, expect } from "vitest";
import { Linter } from "eslint";
import tsParser from "@typescript-eslint/parser";
import rule from "../no-raw-font-sizes.js";

function lint(code: string) {
  const linter = new Linter({ configType: "flat" });
  return linter.verify(code, {
    plugins: { local: { rules: { "no-raw-font-sizes": rule } } },
    languageOptions: {
      parser: tsParser,
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    rules: { "local/no-raw-font-sizes": "error" },
  });
}

test("flags arbitrary px font sizes", () => {
  expect(lint(`const x = <div className="text-[10px]" />;`)).toHaveLength(1);
  expect(lint(`const x = <div className="text-[11px]" />;`)).toHaveLength(1);
  expect(lint(`const x = <div className="text-[13px]" />;`)).toHaveLength(1);
});

test("flags arbitrary rem font sizes", () => {
  expect(lint(`const x = <div className="text-[0.875rem]" />;`)).toHaveLength(1);
});

test("flags font sizes inside template literals", () => {
  const msgs = lint("const c = `text-[9px] leading-none`;");
  expect(msgs).toHaveLength(1);
});

test("allows design-system font tokens", () => {
  const msgs = lint(
    `const x = <div className="text-3xs text-2xs text-code text-message text-xs text-sm" />;`,
  );
  expect(msgs).toHaveLength(0);
});

test("does not flag non-font arbitrary values", () => {
  // Arbitrary dims, opacity, etc. are outside this rule's scope (the
  // page-scoped no-raw-design-values rule governs those).
  const msgs = lint(`const x = <div className="max-h-[300px] opacity-[0.3]" />;`);
  expect(msgs).toHaveLength(0);
});
