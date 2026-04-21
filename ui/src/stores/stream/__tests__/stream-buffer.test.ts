import { describe, it, expect } from "vitest";
import { StreamBuffer } from "../stream-buffer";

describe("StreamBuffer", () => {
  it("snapshot returns finalized parts plus live parser snapshot", () => {
    const buf = new StreamBuffer(null);
    buf.parts.push({
      type: "tool",
      toolCallId: "t1",
      toolName: "fn",
      state: "output-available" as const,
      input: {},
      output: "ok",
    });
    buf.parser.processDelta("hello");
    const snap = buf.snapshot();
    expect(snap).toHaveLength(2);
    expect(snap[0].type).toBe("tool");
    expect(snap[1].type).toBe("text");
    expect((snap[1] as any).text).toContain("hello");
  });

  it("reasoning is NOT lost after flushText + tool push (bug a regression)", () => {
    const buf = new StreamBuffer(null);
    buf.parser.processDelta("<think>deep thought</think>and then");
    buf.flushText();
    buf.parts.push({
      type: "tool",
      toolCallId: "t2",
      toolName: "search",
      state: "input-streaming" as const,
      input: {},
    });
    const snap = buf.snapshot();
    const reasoning = snap.find(p => p.type === "reasoning");
    const text = snap.find(p => p.type === "text");
    const tool = snap.find(p => p.type === "tool");
    expect(reasoning).toBeDefined();
    expect((reasoning as any).text).toContain("deep thought");
    expect(text).toBeDefined();
    expect(tool).toBeDefined();
  });

  it("snapshot has no side-effects — calling it twice returns same content", () => {
    const buf = new StreamBuffer(null);
    buf.parser.processDelta("partial text");
    const s1 = buf.snapshot();
    const s2 = buf.snapshot();
    expect(s1).toEqual(s2);
    buf.parser.processDelta(" more");
    const s3 = buf.snapshot();
    expect((s3[0] as any).text).toContain("partial text more");
  });

  it("reset clears parts, resets parser, generates new assistantId", () => {
    const buf = new StreamBuffer("agent1");
    buf.parts.push({
      type: "tool",
      toolCallId: "t1",
      toolName: "fn",
      state: "input-streaming" as const,
      input: {},
    });
    buf.parser.processDelta("hello");
    const oldId = buf.assistantId;
    buf.reset();
    expect(buf.parts).toHaveLength(0);
    expect(buf.snapshot()).toHaveLength(0);
    expect(buf.assistantId).not.toBe(oldId);
    expect(buf.toolInputChunks.size).toBe(0);
  });

  it("reset preserves currentRespondingAgent", () => {
    const buf = new StreamBuffer("agentA");
    buf.reset();
    expect(buf.currentRespondingAgent).toBe("agentA");
  });

  it("flushText moves parser content into parts without side-effects on further deltas", () => {
    const buf = new StreamBuffer(null);
    buf.parser.processDelta("first");
    buf.flushText();
    expect(buf.parts).toHaveLength(1);
    expect((buf.parts[0] as any).text).toContain("first");
    buf.parser.processDelta("second");
    const snap = buf.snapshot();
    expect(snap).toHaveLength(2);
  });

  it("two flushText calls do not duplicate text (multi-tool regression)", () => {
    // Regression: before fix, flush() didn't clear parser.parts, so the second
    // flushText() would re-push the first segment into buffer.parts, causing
    // the same text to appear twice in the rendered message.
    const buf = new StreamBuffer(null);

    // First text block (long enough to pass the 15-char safe buffer in IncrementalParser)
    buf.parser.processDelta("Text before first tool call - definitely long enough to emit.");
    buf.flushText(); // simulates tool-input-start

    // Second text block after first tool
    buf.parser.processDelta("Text before second tool call - also long enough to emit here.");
    buf.flushText(); // simulates second tool-input-start

    const snap = buf.snapshot();
    // Each segment should appear exactly once
    const allText = snap
      .filter(p => p.type === "text")
      .map(p => (p as any).text as string)
      .join("||SEP||");

    // "Text before first" must appear exactly once
    const firstCount = (allText.match(/Text before first/g) ?? []).length;
    expect(firstCount).toBe(1);

    // "Text before second" must appear exactly once
    const secondCount = (allText.match(/Text before second/g) ?? []).length;
    expect(secondCount).toBe(1);
  });
});
