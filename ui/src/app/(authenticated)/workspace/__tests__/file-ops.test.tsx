import { describe, it, expect } from "vitest";
import { buildRenameTarget } from "@/app/(authenticated)/workspace/file-ops";

describe("buildRenameTarget", () => {
  it("keeps file in current folder", () => {
    expect(buildRenameTarget("zettelkasten/Note", "a.md", "b.md")).toEqual({
      from: "zettelkasten/Note/a.md",
      to: "zettelkasten/Note/b.md",
    });
  });
  it("works at root", () => {
    expect(buildRenameTarget("", "a.md", "b.md")).toEqual({ from: "a.md", to: "b.md" });
  });
});
