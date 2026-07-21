import { describe, it, expect } from "vitest";
import { resolveAssetPath, findImageMatches } from "@/components/workspace/md-decorations/images";

describe("resolveAssetPath", () => {
  it("resolves relative against noteDir", () => {
    expect(resolveAssetPath("vault/Note", "images/x.png")).toBe("vault/Note/images/x.png");
  });
  it("returns null for absolute urls", () => {
    expect(resolveAssetPath("vault/Note", "https://e.com/a.png")).toBeNull();
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
    expect(m[0].alt).toBe("alt");
  });
  it("handles image src with spaces (local paths)", () => {
    const m = findImageMatches("![a](images/my photo.png)");
    expect(m).toHaveLength(1);
    expect(m[0].src).toBe("images/my photo.png");
  });
  it("skips image with empty src", () => {
    const m = findImageMatches("![]()");
    expect(m).toHaveLength(0);
  });
});
