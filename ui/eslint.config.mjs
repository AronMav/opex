import { defineConfig, globalIgnores } from "eslint/config";
import nextVitals from "eslint-config-next/core-web-vitals";
import nextTs from "eslint-config-next/typescript";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const noRawDesignValues = require("./eslint-rules/no-raw-design-values.js");

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
    ],
    plugins: { local: { rules: { "no-raw-design-values": noRawDesignValues } } },
    rules: { "local/no-raw-design-values": "error" },
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
