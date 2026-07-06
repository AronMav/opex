import { defineConfig, globalIgnores } from "eslint/config";
import nextVitals from "eslint-config-next/core-web-vitals";
import nextTs from "eslint-config-next/typescript";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const noRawDesignValues = require("./eslint-rules/no-raw-design-values.js");
const noRawFontSizes = require("./eslint-rules/no-raw-font-sizes.js");

const eslintConfig = defineConfig([
  ...nextVitals,
  ...nextTs,
  {
    rules: {
      "react-hooks/set-state-in-effect": "off",
      "react-hooks/purity": "off",
      "react-hooks/preserve-manual-memoization": "warn",
      "react-hooks/refs": "warn",
    },
  },
  // Relax type-strictness rules in test files (mocks, fixtures, dynamic requires)
  {
    files: ["**/__tests__/**", "**/__e2e__/**", "**/*.test.{ts,tsx}", "**/*.spec.{ts,tsx}"],
    rules: {
      "@typescript-eslint/no-explicit-any": "off",
      "@typescript-eslint/no-require-imports": "off",
      "react-hooks/globals": "off",
      "react-hooks/preserve-manual-memoization": "off",
      "react-hooks/refs": "off",
    },
  },
  // Local design-system rules. The "local" plugin is registered ONCE here
  // (flat config forbids redefining the same plugin name across blocks); the
  // individual rules are activated with different file scopes below.
  {
    plugins: {
      local: {
        rules: {
          "no-raw-design-values": noRawDesignValues,
          "no-raw-font-sizes": noRawFontSizes,
        },
      },
    },
  },
  // Design-system guard — enforced ONLY on migrated pages. Add globs here as
  // each page batch lands; goal is `src/app/**` once migration completes.
  {
    files: [
      "src/app/(authenticated)/webhooks/**/*.{ts,tsx}",
      "src/app/(authenticated)/secrets/**/*.{ts,tsx}",
      "src/app/(authenticated)/backups/**/*.{ts,tsx}",
      "src/app/(authenticated)/channels/**/*.{ts,tsx}",
      "src/app/(authenticated)/integrations/**/*.{ts,tsx}",
      "src/app/(authenticated)/tasks/**/*.{ts,tsx}",
      "src/app/(authenticated)/providers/**/*.{ts,tsx}",
      "src/app/(authenticated)/agents/**/*.{ts,tsx}",
      "src/app/(authenticated)/monitor/**/*.{ts,tsx}",
      "src/app/(authenticated)/config/**/*.{ts,tsx}",
      "src/app/(authenticated)/skills/**/*.{ts,tsx}",
      "src/app/(authenticated)/memory/**/*.{ts,tsx}",
      "src/app/(authenticated)/access/**/*.{ts,tsx}",
      "src/app/login/**/*.{ts,tsx}",
      "src/app/setup/**/*.{ts,tsx}",
      "src/app/error.tsx",
      "src/app/(authenticated)/error.tsx",
      "src/app/(authenticated)/tools/**/*.{ts,tsx}",
      "src/app/(authenticated)/chat/**/*.{ts,tsx}",
      "src/app/(authenticated)/workspace/**/*.{ts,tsx}",
      "src/app/(authenticated)/canvas/**/*.{ts,tsx}",
    ],
    rules: { "local/no-raw-design-values": "error" },
  },
  // Font-size token guard — applies app-wide (primitives, feature components,
  // pages). The font-size token scale is established in globals.css
  // (text-3xs / text-2xs / text-code / text-message + the Tailwind scale), so
  // arbitrary `text-[Npx]` / `text-[Nrem]` values are always a regression.
  {
    files: ["src/**/*.{ts,tsx}"],
    rules: { "local/no-raw-font-sizes": "error" },
  },
  // Override default ignores of eslint-config-next.
  globalIgnores([
    // Default ignores of eslint-config-next:
    ".next/**",
    "out/**",
    "build/**",
    "next-env.d.ts",
  ]),
]);

export default eslintConfig;
