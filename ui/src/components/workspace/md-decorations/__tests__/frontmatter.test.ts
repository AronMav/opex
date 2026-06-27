import { describe, it, expect } from "vitest";
import { frontmatterRange } from "@/components/workspace/md-decorations/frontmatter";

describe("frontmatterRange", () => {
  it("detects leading frontmatter block", () => {
    const doc = "---\ntitle: x\ntags: [a]\n---\n\n# Body";
    const r = frontmatterRange(doc)!;
    expect(doc.slice(r.from, r.to)).toBe("---\ntitle: x\ntags: [a]\n---");
  });
  it("returns null when no frontmatter", () => {
    expect(frontmatterRange("# Just body")).toBeNull();
  });
});
