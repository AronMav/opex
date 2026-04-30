// Unit tests for getContextLimit() in model-limits.ts

import { describe, it, expect } from "vitest";
import { getContextLimit, MODEL_CONTEXT_LIMITS } from "@/lib/model-limits";

describe("getContextLimit", () => {
  it("returns null for null/undefined/empty", () => {
    expect(getContextLimit(null)).toBeNull();
    expect(getContextLimit(undefined)).toBeNull();
    expect(getContextLimit("")).toBeNull();
  });

  it("returns the correct limit for an exact match", () => {
    expect(getContextLimit("gpt-4o")).toBe(128_000);
    expect(getContextLimit("claude-opus-4")).toBe(200_000);
    expect(getContextLimit("gemini-2.5-pro")).toBe(1_048_576);
  });

  it("is case-insensitive", () => {
    expect(getContextLimit("GPT-4O")).toBe(128_000);
    expect(getContextLimit("Claude-Sonnet-4")).toBe(200_000);
    expect(getContextLimit("GEMINI-2.0-FLASH")).toBe(1_048_576);
  });

  it("matches via prefix for version-suffixed model names", () => {
    // "claude-sonnet-4-20250501" should match "claude-sonnet-4" key
    expect(getContextLimit("claude-sonnet-4-20250501")).toBe(200_000);
    // "gpt-4o-2024-08-06" should match "gpt-4o" key
    expect(getContextLimit("gpt-4o-2024-08-06")).toBe(128_000);
  });

  it("longer key wins over shorter when both are prefixes", () => {
    // "claude-opus-4.7" is more specific than "claude-opus-4"
    // A model named exactly "claude-opus-4.7" should get 1_000_000
    expect(getContextLimit("claude-opus-4.7")).toBe(1_000_000);
    // A model "claude-opus-4" (no .7) should get 200_000
    expect(getContextLimit("claude-opus-4")).toBe(200_000);
  });

  it("returns null for unknown model", () => {
    expect(getContextLimit("unknown-model-xyz")).toBeNull();
    expect(getContextLimit("llama-3.1-70b")).toBeNull();
  });

  it("MODEL_CONTEXT_LIMITS contains the expected keys", () => {
    expect(MODEL_CONTEXT_LIMITS["gpt-4o"]).toBe(128_000);
    expect(MODEL_CONTEXT_LIMITS["claude-sonnet-4"]).toBe(200_000);
    expect(MODEL_CONTEXT_LIMITS["gemini-2.5-flash"]).toBe(1_048_576);
  });
});
