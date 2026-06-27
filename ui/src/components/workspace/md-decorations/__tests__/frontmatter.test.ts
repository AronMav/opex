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
  it("returns null when frontmatter is not at position 0", () => {
    const doc = "\n---\ntitle: x\n---\n";
    expect(frontmatterRange(doc)).toBeNull();
  });
  it("detects CRLF frontmatter block", () => {
    const doc = "---\r\ntitle: x\r\n---\r\n\r\n# Body";
    const r = frontmatterRange(doc)!;
    expect(r).not.toBeNull();
    expect(r.from).toBe(0);
    // Slice should start with --- and end with ---
    expect(doc.slice(r.from, r.to)).toMatch(/^---\r?\n[\s\S]*?\r?\n---$/);
  });
});
