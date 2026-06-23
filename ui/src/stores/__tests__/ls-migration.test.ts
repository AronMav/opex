import { describe, it, expect, beforeEach } from "vitest";
import { readWithLegacy } from "@/stores/ls-migration";

describe("readWithLegacy", () => {
  beforeEach(() => localStorage.clear());
  it("returns new key when present", () => {
    localStorage.setItem("opex.auth.token", "new");
    localStorage.setItem("hydeclaw.auth.token", "old");
    expect(readWithLegacy("opex.auth.token", "hydeclaw.auth.token")).toBe("new");
  });
  it("falls back to legacy and migrates", () => {
    localStorage.setItem("hydeclaw.auth.token", "old");
    expect(readWithLegacy("opex.auth.token", "hydeclaw.auth.token")).toBe("old");
    expect(localStorage.getItem("opex.auth.token")).toBe("old"); // мигрирует
  });
});
