import { describe, it, expect } from "vitest";
import { frontmatterRange, frontmatterEndLine } from "@/components/workspace/md-decorations/frontmatter";

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

describe("frontmatterEndLine", () => {
  it("returns 3 for a minimal LF frontmatter block (line arrays with stripped endings)", () => {
    // CM's line(n).text strips \r, so we pass pre-stripped lines
    const lines = ["---", "k: v", "---"];
    expect(frontmatterEndLine(lines)).toBe(3);
  });

  it("returns 3 for lines that would come from a CRLF document (already stripped by CM)", () => {
    // CM strips \r before returning .text, so CRLF and LF produce identical line arrays
    const lines = ["---", "title: hello", "---", "", "# Body"];
    expect(frontmatterEndLine(lines)).toBe(3);
  });

  it("returns null when first line is not ---", () => {
    const lines = ["# heading", "---", "k: v", "---"];
    expect(frontmatterEndLine(lines)).toBeNull();
  });

  it("returns null when there is no closing ---", () => {
    const lines = ["---", "k: v", "still content"];
    expect(frontmatterEndLine(lines)).toBeNull();
  });

  it("returns null when closing --- is beyond maxLines cap", () => {
    // closing --- at index 51 (line 52), cap is 50
    const lines = ["---", ...Array(50).fill("k: v"), "---"];
    expect(frontmatterEndLine(lines, 50)).toBeNull();
  });

  it("finds closing --- exactly at the maxLines boundary (index maxLines-1)", () => {
    // lines[0] = "---", lines[49] = "---" → line 50 → within cap of 50
    const lines = ["---", ...Array(48).fill("k: v"), "---"];
    expect(frontmatterEndLine(lines, 50)).toBe(50);
  });
});
