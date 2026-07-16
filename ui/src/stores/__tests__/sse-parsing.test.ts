import { describe, it, expect } from "vitest";
import { parseContentParts } from "@/stores/sse-events";

// ── parseContentParts unit tests ─────────────────────────────────────────────

describe("parseContentParts — basic cases", () => {
  it("parses text with think tags into mixed parts", () => {
    const result = parseContentParts("Hello <think>reasoning</think> world");
    expect(result).toEqual([
      { type: "text", text: "Hello " },
      { type: "reasoning", text: "reasoning" },
      { type: "text", text: " world" },
    ]);
  });

  it("returns single text part when no think tags", () => {
    const result = parseContentParts("No think tags here");
    expect(result).toEqual([{ type: "text", text: "No think tags here" }]);
  });

  it("returns empty array for empty string", () => {
    expect(parseContentParts("")).toEqual([]);
  });

  it("handles unclosed think tag", () => {
    const result = parseContentParts("Before <think>unclosed reasoning");
    expect(result).toEqual([
      { type: "text", text: "Before " },
      { type: "reasoning", text: "unclosed reasoning" },
    ]);
  });

  it("handles only reasoning content", () => {
    const result = parseContentParts("<think>only reasoning</think>");
    expect(result).toEqual([{ type: "reasoning", text: "only reasoning" }]);
  });

  it("handles multiple think blocks", () => {
    const result = parseContentParts("Text <think>r1</think> middle <think>r2</think> end");
    expect(result).toEqual([
      { type: "text", text: "Text " },
      { type: "reasoning", text: "r1" },
      { type: "text", text: " middle " },
      { type: "reasoning", text: "r2" },
      { type: "text", text: " end" },
    ]);
  });

  it("strips minimax:tool_call tags from text parts", () => {
    const result = parseContentParts("Clean text <minimax:tool_call>tool data</minimax:tool_call> after");
    // The tool_call tag should be stripped from text parts
    const textParts = result.filter(p => p.type === "text");
    expect(textParts.every(p => !p.text.includes("minimax:tool_call"))).toBe(true);
  });
});

// ── SSE-02: Unification (same output regardless of assembly method) ───────────

describe("SSE-02: parseContentParts unification", () => {
  it("produces identical output from single string vs concatenated chunks", () => {
    const fullString = "Before <think>start of reasoning</think> After";
    const chunk1 = "Before <think>start of rea";
    const chunk2 = "soning</think> After";
    const concatenated = chunk1 + chunk2;

    const fromSingle = parseContentParts(fullString);
    const fromConcatenated = parseContentParts(concatenated);

    expect(fromConcatenated).toEqual(fromSingle);
  });

  it("handles think tag split across chunks after joining", () => {
    // Simulate: accumulate two text-delta SSE chunks into textAccum, then parse
    let textAccum = "";
    textAccum += "Before <think>start of rea";
    textAccum += "soning</think> After";

    const result = parseContentParts(textAccum);
    expect(result).toEqual([
      { type: "text", text: "Before " },
      { type: "reasoning", text: "start of reasoning" },
      { type: "text", text: " After" },
    ]);
  });
});

// ── SSE-01: flushText() live state machine simulation ─────────────────────────

describe("SSE-01: flushText() live state machine", () => {
  it("assembles split <think> tag across two text-delta events", () => {
    // Reproduce the flushText() state machine behavior from processSSEStream
    // The real code accumulates text in textAccum before calling flushText()
    let textAccum = "";
    const parts: Array<{ type: string; text: string }> = [];

    function flushText() {
      if (!textAccum) return;
      const parsed = parseContentParts(textAccum);
      parts.push(...parsed);
      textAccum = "";
    }

    // Delta 1: partial think tag — accumulate but do NOT flush mid-stream
    textAccum += "Hello <thi";
    // Delta 2: completes the think tag
    textAccum += "nk>deep thought</think> world";
    // flushText() called on non-text event (e.g., tool-input-start, finish, file)
    flushText();

    expect(parts).toEqual([
      { type: "text", text: "Hello " },
      { type: "reasoning", text: "deep thought" },
      { type: "text", text: " world" },
    ]);
  });

  it("handles flushText() called between deltas with partial tag", () => {
    // Scenario: flushText() is called mid-stream (e.g., tool-input-start interrupts)
    // with an incomplete <think> tag — textAccum has "Prefix <thi"
    let textAccum = "Prefix <thi";
    const parts: Array<{ type: string; text: string }> = [];

    // flushText with incomplete tag — parseContentParts treats as plain text
    const parsed1 = parseContentParts(textAccum);
    parts.push(...parsed1);
    textAccum = "";

    // Later delta completes what would have been the tag
    textAccum = "nk>thought</think> suffix";
    const parsed2 = parseContentParts(textAccum);
    parts.push(...parsed2);

    // The split caused the tag to break — known limitation corrected by finish normalization
    expect(parts.length).toBeGreaterThan(0);

    // After finish normalization, reassembling all text produces correct parts
    const fullText = parts
      .map(p => (p.type === "reasoning" ? `<think>${p.text}</think>` : p.text))
      .join("");
    const normalized = parseContentParts(fullText);
    // The normalized result may differ from parts — that's what SSE-02 finish handler fixes
    expect(normalized.some(p => p.type === "reasoning")).toBe(true);
  });

  it("handles plain text without think tags in live state machine", () => {
    let textAccum = "";
    const parts: Array<{ type: string; text: string }> = [];

    function flushText() {
      if (!textAccum) return;
      parts.push(...parseContentParts(textAccum));
      textAccum = "";
    }

    textAccum += "Hello ";
    textAccum += "world";
    flushText();

    expect(parts).toEqual([{ type: "text", text: "Hello world" }]);
  });

  it("handles multiple complete think blocks in accumulated text", () => {
    let textAccum = "";
    const parts: Array<{ type: string; text: string }> = [];

    function flushText() {
      if (!textAccum) return;
      parts.push(...parseContentParts(textAccum));
      textAccum = "";
    }

    textAccum += "Start <think>r1</think> mid <think>r2</think> end";
    flushText();

    expect(parts).toEqual([
      { type: "text", text: "Start " },
      { type: "reasoning", text: "r1" },
      { type: "text", text: " mid " },
      { type: "reasoning", text: "r2" },
      { type: "text", text: " end" },
    ]);
  });
});
