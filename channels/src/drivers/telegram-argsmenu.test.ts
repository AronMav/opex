import { describe, it, expect } from "bun:test";
import { parseCmCallback } from "./telegram-argsmenu";

describe("parseCmCallback", () => {
  it("parses token + value", () => {
    expect(parseCmCallback("cm:abc:long")).toEqual({ token: "abc", value: "long" });
  });

  it("rejoins a value that itself contains a colon", () => {
    expect(parseCmCallback("cm:abc:ru:extra")).toEqual({ token: "abc", value: "ru:extra" });
  });

  it("returns null for non-cm callback data", () => {
    expect(parseCmCallback("hm:abc:handler")).toBeNull();
    expect(parseCmCallback("approve:uuid")).toBeNull();
    expect(parseCmCallback("")).toBeNull();
  });

  it("returns null when token or value is missing", () => {
    expect(parseCmCallback("cm:")).toBeNull();
    expect(parseCmCallback("cm:abc")).toBeNull();
    expect(parseCmCallback("cm::value")).toBeNull();
    expect(parseCmCallback("cm:abc:")).toBeNull();
  });
});
