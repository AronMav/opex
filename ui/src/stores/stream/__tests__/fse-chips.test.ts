import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

describe("stream-processor file-scenario-chips", () => {
  it("has a case for file-scenario-chips", () => {
    const src = readFileSync(path.join(__dirname, "../stream-processor.ts"), "utf8");
    expect(src).toContain('case "file-scenario-chips"');
    expect(src).toContain("fileScenarioChips");
  });
});
