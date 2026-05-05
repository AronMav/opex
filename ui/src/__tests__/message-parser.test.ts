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

describe("IncrementalParser — split tag boundaries (dynamic holdback)", () => {
  it("opening tag split mid-tag across two deltas: <thi + nk>", () => {
    const parser = new IncrementalParser();
    parser.processDelta("hello <thi");
    parser.processDelta("nk>secret reasoning content goes here</think> after");
    const result = parser.flush();
    const reasoning = result.filter(p => p.type === "reasoning").map(p => p.text).join("");
    const text = result.filter(p => p.type === "text").map(p => p.text).join("");
    expect(reasoning).toContain("secret reasoning content");
    expect(text).toContain("hello");
    expect(text).toContain("after");
  });

  it("closing tag split mid-tag: </thi + nk>", () => {
    const parser = new IncrementalParser();
    parser.processDelta("<think>internal stuff");
    parser.processDelta(" continues</thi");
    parser.processDelta("nk>visible response");
    const result = parser.flush();
    const reasoning = result.filter(p => p.type === "reasoning").map(p => p.text).join("");
    const text = result.filter(p => p.type === "text").map(p => p.text).join("");
    expect(reasoning).toContain("internal stuff");
    expect(reasoning).toContain("continues");
    expect(text).toContain("visible response");
    // Closing tag must NOT leak into either text or reasoning
    expect(reasoning).not.toContain("</");
    expect(text).not.toContain("</");
  });

  it("longest tag </antthinking> split across three chunks", () => {
    const parser = new IncrementalParser();
    parser.processDelta("<antthinking>deep thought");
    parser.processDelta("</ant");
    parser.processDelta("thinking>final");
    const result = parser.flush();
    const reasoning = result.filter(p => p.type === "reasoning").map(p => p.text).join("");
    const text = result.filter(p => p.type === "text").map(p => p.text).join("");
    expect(reasoning).toContain("deep thought");
    expect(text).toContain("final");
    expect(reasoning).not.toContain("</");
  });

  it("releases held-back text when chunk extends without forming a tag", () => {
    const parser = new IncrementalParser();
    // "<th" looks like a partial <think> opening — held back at first
    parser.processDelta("plain text<th");
    // Next chunk: "i is not a tag" — "<thi" would still be a partial prefix of <think>
    // but "<thi " (with space after) cannot become a valid tag any more
    parser.processDelta("i is not a tag here at all");
    const snap = parser.snapshot();
    const text = snap.filter(p => p.type === "text").map(p => p.text).join("");
    // The "<thi" must eventually appear as plain text
    expect(text).toContain("plain text");
    expect(text).toContain("not a tag");
  });

  it("does NOT hold back text without any partial-tag suffix", () => {
    const parser = new IncrementalParser();
    parser.processDelta("a long sentence with no angle brackets at all here");
    // Without partial tag suffix, holdback is 0 — accum drained immediately
    const snap = parser.snapshot();
    const text = snap.filter(p => p.type === "text").map(p => p.text).join("");
    expect(text).toBe("a long sentence with no angle brackets at all here");
  });

  it("emits short text without trailing < as plain text once flushed", () => {
    const parser = new IncrementalParser();
    // Trailing "<" is held back — it could be the start of a tag
    parser.processDelta("hi<");
    const result = parser.flush();
    const text = result.filter(p => p.type === "text").map(p => p.text).join("");
    expect(text).toBe("hi<");
  });
});
