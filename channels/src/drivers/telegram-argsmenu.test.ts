import { describe, it, expect } from "bun:test";
import { parseCmCallback, parseHmCallback } from "./telegram-argsmenu";

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

describe("parseHmCallback", () => {
  it("parses token + handler_id", () => {
    expect(parseHmCallback("hm:tok:summarize_video")).toEqual({ token: "tok", handlerId: "summarize_video" });
  });

  it("rejoins a handler_id containing a colon", () => {
    expect(parseHmCallback("hm:tok:ns:handler")).toEqual({ token: "tok", handlerId: "ns:handler" });
  });

  it("returns null for non-hm data and for missing token/handler_id", () => {
    expect(parseHmCallback("cm:abc:long")).toBeNull();
    expect(parseHmCallback("")).toBeNull();
    expect(parseHmCallback("hm:")).toBeNull();
    expect(parseHmCallback("hm:tok")).toBeNull();
    expect(parseHmCallback("hm::handler")).toBeNull();
    expect(parseHmCallback("hm:tok:")).toBeNull();
  });
});
