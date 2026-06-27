import { describe, it, expect } from "vitest";
import { isBinaryFile } from "@/lib/api";

describe("isBinaryFile", () => {
  it("narrows binary responses", () => {
    expect(isBinaryFile({ is_binary: true, mime: "image/png", size: 1, url: "/x", path: "x.png", is_dir: false })).toBe(true);
    expect(isBinaryFile({ content: "hi", path: "n.md", is_dir: false })).toBe(false);
  });
});
