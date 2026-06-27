import { describe, it, expect } from "vitest";
import { resolveAssetPath, findImageMatches } from "@/components/workspace/md-decorations/images";

describe("resolveAssetPath", () => {
  it("resolves relative against noteDir", () => {
    expect(resolveAssetPath("zettelkasten/Note", "images/x.png")).toBe("zettelkasten/Note/images/x.png");
  });
  it("returns null for absolute urls", () => {
    expect(resolveAssetPath("zettelkasten/Note", "https://e.com/a.png")).toBeNull();
  });
  it("returns null for root-relative urls", () => {
    expect(resolveAssetPath("note", "/images/x.png")).toBeNull();
  });
});

describe("findImageMatches", () => {
  it("finds standard markdown images", () => {
    const m = findImageMatches("text ![alt](images/x.png) more");
    expect(m).toHaveLength(1);
    expect(m[0].src).toBe("images/x.png");
  });
});
