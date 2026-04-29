import { describe, it, expect } from "vitest";

import { isActivePhase } from "@/stores/chat-store";
import { uuid } from "@/stores/chat-types";

describe("isActivePhase", () => {
  it("returns true for active streaming states", () => {
    expect(isActivePhase("submitted")).toBe(true);
    expect(isActivePhase("streaming")).toBe(true);
    expect(isActivePhase("reconnecting")).toBe(true);
  });

  it("returns false for idle/error/complete states", () => {
    expect(isActivePhase("idle")).toBe(false);
    expect(isActivePhase("error")).toBe(false);
    expect(isActivePhase("complete")).toBe(false);
    expect(isActivePhase(undefined)).toBe(false);
  });
});

describe("uuid() helper", () => {
  it("produces valid UUID v4 strings", () => {
    // In jsdom (secure context), uuid() delegates to crypto.randomUUID().
    const UUID_V4_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
    expect(uuid()).toMatch(UUID_V4_RE);
  });

  it("produces unique values on successive calls", () => {
    expect(uuid()).not.toBe(uuid());
  });
});
