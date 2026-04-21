import { describe, it, expect } from "vitest";
import { IncrementalParser } from "@/lib/message-parser";

describe("IncrementalParser.flush()", () => {
  it("does not re-return previously flushed parts on second flush (duplication regression)", () => {
    const parser = new IncrementalParser();
    // Simulate first text segment (enough to exceed the 15-char safe buffer)
    parser.processDelta("This is the first text segment that is long enough.");
    const first = parser.flush();
    expect(first.length).toBeGreaterThan(0);
    const firstText = first.map(p => p.text).join("");

    // Simulate second text segment
    parser.processDelta("Second segment.");
    const second = parser.flush();
    // Second flush must NOT contain text from the first flush
    const secondText = second.map(p => p.text).join("");
    expect(secondText).not.toContain(firstText.slice(0, 20));
    // And must contain the new text
    expect(secondText).toContain("Second");
  });

  it("resets parts after flush — subsequent snapshot shows only new content", () => {
    const parser = new IncrementalParser();
    parser.processDelta("initial content that is definitely long enough to process");
    parser.flush();
    // After flush, snapshot should return empty (no new deltas)
    const snap = parser.snapshot();
    expect(snap).toHaveLength(0);
  });
});

describe("IncrementalParser.reset()", () => {
  it("clears insideThink state — text after reset is classified as text, not reasoning", () => {
    const parser = new IncrementalParser();
    // Start a think block but don't close it — parser is now insideThink
    parser.processDelta("<think>partial reasoning");
    // Reset should clear insideThink
    parser.reset();
    // Now feed text — it should be plain text, not reasoning
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
    // After reset, accum is empty — empty delta should produce no new parts
    const parts = parser.processDelta("");
    expect(parts).toEqual([]);
  });

  it("clears parts — flush after reset returns empty", () => {
    const parser = new IncrementalParser();
    // Add substantial text to get something into parts
    parser.processDelta("some text that is long enough to exceed buffer and emit");
    // Reset should clear parts
    parser.reset();
    // Flush should return empty (no accumulated content)
    const flushed = parser.flush();
    expect(flushed).toEqual([]);
  });
});
