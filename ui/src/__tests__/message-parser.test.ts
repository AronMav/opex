import { describe, it, expect } from "vitest";
import { IncrementalParser } from "@/lib/message-parser";

describe("IncrementalParser.flush()", () => {
  it("does not re-return previously flushed parts on second flush (duplication regression)", () => {
    const parser = new IncrementalParser();
    parser.processDelta("This is the first text segment that is long enough.");
    const first = parser.flush();
    expect(first.length).toBeGreaterThan(0);
    const firstText = first.map(p => p.text).join("");

    parser.processDelta("Second segment.");
    const second = parser.flush();
    const secondText = second.map(p => p.text).join("");
    expect(secondText).not.toContain(firstText.slice(0, 20));
    expect(secondText).toContain("Second");
  });

  it("resets parts after flush — subsequent snapshot shows only new content", () => {
    const parser = new IncrementalParser();
    parser.processDelta("initial content that is definitely long enough to process");
    parser.flush();
    const snap = parser.snapshot();
    expect(snap).toHaveLength(0);
  });

  it("flush on empty parser returns empty array", () => {
    const parser = new IncrementalParser();
    expect(parser.flush()).toEqual([]);
  });

  it("flushes remaining accum that is shorter than safe-buffer threshold", () => {
    const parser = new IncrementalParser();
    // 10 chars — below the 15-char safety buffer, stays in accum until flush
    parser.processDelta("short text");
    const result = parser.flush();
    const text = result.map(p => p.text).join("");
    expect(text).toContain("short text");
  });
});

describe("IncrementalParser.reset()", () => {
  it("clears insideThink state — text after reset is classified as text, not reasoning", () => {
    const parser = new IncrementalParser();
    parser.processDelta("<think>partial reasoning");
    parser.reset();
    const parts = parser.processDelta("hello world text here that is long enough");
    const textParts = parts.filter(p => p.type === "text");
    const reasoningParts = parts.filter(p => p.type === "reasoning");
    expect(textParts.length).toBeGreaterThan(0);
    expect(reasoningParts.length).toBe(0);
  });

  it("clears accum — processDelta after reset returns empty parts for empty input", () => {
    const parser = new IncrementalParser();
    parser.processDelta("some buffered text");
    parser.reset();
    const parts = parser.processDelta("");
    expect(parts).toEqual([]);
  });

  it("clears parts — flush after reset returns empty", () => {
    const parser = new IncrementalParser();
    parser.processDelta("some text that is long enough to exceed buffer and emit");
    parser.reset();
    expect(parser.flush()).toEqual([]);
  });
});

describe("IncrementalParser — think tag variants", () => {
  it("recognises <thinking> open/close tag variant", () => {
    const parser = new IncrementalParser();
    parser.processDelta("<thinking>internal reasoning here and more text to exceed buffer limit</thinking>response");
    const result = parser.flush();
    const reasoningParts = result.filter(p => p.type === "reasoning");
    const textParts = result.filter(p => p.type === "text");
    expect(reasoningParts.length).toBeGreaterThan(0);
    expect(textParts.some(p => p.text.includes("response"))).toBe(true);
  });

  it("recognises <antthinking> open/close tag variant", () => {
    const parser = new IncrementalParser();
    parser.processDelta("<antthinking>anthropic thinking here and more text to exceed buffer limit</antthinking>visible");
    const result = parser.flush();
    const reasoningParts = result.filter(p => p.type === "reasoning");
    const textParts = result.filter(p => p.type === "text");
    expect(reasoningParts.length).toBeGreaterThan(0);
    expect(textParts.some(p => p.text.includes("visible"))).toBe(true);
  });
});
