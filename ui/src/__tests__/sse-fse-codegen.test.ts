import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import path from "node:path";

describe("sse.generated.ts FSE codegen", () => {
  it("contains the file-scenario-chips variant and ScenarioChoice", () => {
    const filePath = path.resolve(__dirname, "../types/sse.generated.ts");
    const src = readFileSync(filePath, "utf8");
    expect(src).toContain('"type": "file-scenario-chips"');
    expect(src).toContain("ScenarioChoice");
    expect(src).toContain("uploadId");
  });
});
