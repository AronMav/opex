import { describe, it, expect } from "vitest";
import { buildRenameTarget, encodeWorkspacePath } from "@/app/(authenticated)/workspace/file-ops";

describe("buildRenameTarget", () => {
  it("keeps file in current folder", () => {
    expect(buildRenameTarget("vault/Note", "a.md", "b.md")).toEqual({
      from: "vault/Note/a.md",
      to: "vault/Note/b.md",
    });
  });
  it("works at root", () => {
    expect(buildRenameTarget("", "a.md", "b.md")).toEqual({ from: "a.md", to: "b.md" });
  });
});

describe("encodeWorkspacePath", () => {
  it("encodes spaces", () => {
    expect(encodeWorkspacePath("agents/My Agent/SOUL.md")).toBe(
      "agents/My%20Agent/SOUL.md",
    );
  });

  it("encodes hash characters", () => {
    expect(encodeWorkspacePath("notes/C# guide.md")).toBe(
      "notes/C%23%20guide.md",
    );
  });

  it("encodes deeply nested path", () => {
    expect(encodeWorkspacePath("a/b c/d#e/f.md")).toBe(
      "a/b%20c/d%23e/f.md",
    );
  });

  it("returns empty string unchanged", () => {
    expect(encodeWorkspacePath("")).toBe("");
  });

  it("does not double-encode already plain paths", () => {
    expect(encodeWorkspacePath("plain/path.md")).toBe("plain/path.md");
  });
});
