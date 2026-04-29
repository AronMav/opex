// ui/src/stores/__tests__/stream-state-mutation-grep.test.ts
import { describe, it, expect } from "vitest";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Files allowed to mutate stream-state fields directly.
const ALLOWED_FILES = [
  "stores/stream-session.ts",
  "stores/streaming-renderer.ts",
  "stores/stream/",        // future Phase 4 modules
  "stores/chat-store.ts",  // ensureAgent initial state
  "stores/chat-types.ts",  // type definitions only
  "__tests__/",            // all test files
];

// Patterns that must NOT appear outside ALLOWED_FILES.
// Each regex represents a direct mutation of a stream-state field.
// Assignment patterns (=) catch writes; we deliberately do NOT flag
// object-literal `field: value` shapes used in state initialization
// inside chat-store.ts.
const FORBIDDEN_PATTERNS: RegExp[] = [
  /\bmessageSource\s*=\s*\{[^}]*mode\s*:\s*["']live["']/,
  /\.messageSource\.messages\.push/,
  /\.streamGeneration\s*(?:=|\+=|\+\+)/,
  // Any connectionPhase assignment — "idle", "streaming",
  // "reconnecting", "error" all belong to the stream lifecycle.
  /\.connectionPhase\s*=\s*["']\w+["']/,
  /\.streamError\s*=/,
  /\.connectionError\s*=/,
  /\.reconnectAttempt\s*=/,
];

function walk(dir: string, files: string[] = []): string[] {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === ".next" || entry.name === "out") continue;
      walk(p, files);
    } else if (/\.(ts|tsx)$/.test(entry.name)) {
      files.push(p);
    }
  }
  return files;
}

function isAllowed(relPath: string): boolean {
  return ALLOWED_FILES.some((allowed) => relPath.includes(allowed));
}

describe("stream-state mutation — forbidden outside allow-list", () => {
  it("no forbidden pattern exists outside ALLOWED_FILES", () => {
    const here = path.dirname(fileURLToPath(import.meta.url));
    // stores/__tests__/ → go up to ui/src/
    const srcRoot = path.resolve(here, "..", "..");
    const files = walk(srcRoot);
    const violations: Array<{ file: string; pattern: string; line: number; text: string }> = [];

    for (const file of files) {
      const rel = path.relative(srcRoot, file).replace(/\\/g, "/");
      if (isAllowed(rel)) continue;
      const content = fs.readFileSync(file, "utf8");
      const lines = content.split("\n");
      for (let i = 0; i < lines.length; i++) {
        // Skip comment lines — patterns in comments are documentation, not mutations.
        const trimmed = lines[i].trimStart();
        if (trimmed.startsWith("//") || trimmed.startsWith("*")) continue;
        for (const pat of FORBIDDEN_PATTERNS) {
          if (pat.test(lines[i])) {
            violations.push({ file: rel, pattern: pat.source, line: i + 1, text: trimmed.slice(0, 120) });
          }
        }
      }
    }

    expect(violations, `Forbidden direct stream-state mutations:\n${JSON.stringify(violations, null, 2)}`).toEqual([]);
  });
});
