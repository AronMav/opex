import { describe, it, expect } from "vitest";
import { truncateOutput } from "@/lib/format";

describe("truncateOutput", () => {
  it("returns full text when under limit", () => {
    const result = truncateOutput("hello", 10_000);
    expect(result.truncated).toBe(false);
    expect(result.text).toBe("hello");
    expect(result.hiddenChars).toBe(0);
  });

  it("truncates text over limit", () => {
    const big = "x".repeat(15_000);
    const result = truncateOutput(big, 10_000);
    expect(result.truncated).toBe(true);
    expect(result.text.length).toBe(10_000);
    expect(result.hiddenChars).toBe(5_000);
  });

  it("reports hidden chars correctly", () => {
    const result = truncateOutput("a".repeat(12_345), 10_000);
    expect(result.hiddenChars).toBe(2_345);
  });

  it("text exactly at limit is not truncated", () => {
    const result = truncateOutput("a".repeat(10_000), 10_000);
    expect(result.truncated).toBe(false);
    expect(result.hiddenChars).toBe(0);
  });

  it("empty string is not truncated", () => {
    const result = truncateOutput("", 10_000);
    expect(result.truncated).toBe(false);
    expect(result.text).toBe("");
    expect(result.hiddenChars).toBe(0);
  });

  it("limit of 0 truncates all text", () => {
    const result = truncateOutput("hello", 0);
    expect(result.truncated).toBe(true);
    expect(result.text).toBe("");
    expect(result.hiddenChars).toBe(5);
  });

  it("preserves unicode multibyte characters within limit", () => {
    const emoji = "😀".repeat(5); // 5 emoji × 2 code units = 10 JS chars
    const result = truncateOutput(emoji, 20);
    expect(result.truncated).toBe(false);
    expect(result.text).toBe(emoji);
  });

  it("truncated text starts from the beginning", () => {
    const result = truncateOutput("ABCDE", 3);
    expect(result.text).toBe("ABC");
    expect(result.hiddenChars).toBe(2);
  });
});
