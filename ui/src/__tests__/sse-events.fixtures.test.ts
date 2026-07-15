import { describe, test, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import type { SseEvent } from "@/stores/sse-events";

const __dirname_local = dirname(fileURLToPath(import.meta.url));
const FIXTURES = join(__dirname_local, "fixtures", "sse");

describe("S6.5 Rust → TS round-trip via SSE fixtures", () => {
  test("data-session-id parses with correct shape", () => {
    const raw = readFileSync(join(FIXTURES, "data-session-id.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    expect(parsed.type).toBe("data-session-id");
    if (parsed.type !== "data-session-id") throw new Error("unreachable");
    expect(parsed.data.sessionId).toBe("sess-abc-123");
    expect(parsed.data.contextLimit).toBe(8000);
    expect(parsed.transient).toBe(true);
  });

  test("text-delta parses with id + delta", () => {
    const raw = readFileSync(join(FIXTURES, "text-delta.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    expect(parsed.type).toBe("text-delta");
    if (parsed.type !== "text-delta") throw new Error("unreachable");
    expect(parsed.id).toBe("text-1");
    expect(parsed.delta).toBe("Hello, world");
  });

  test("tool-input-start parses with parallelBatchId optional", () => {
    const raw = readFileSync(join(FIXTURES, "tool-input-start.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    expect(parsed.type).toBe("tool-input-start");
    if (parsed.type !== "tool-input-start") throw new Error("unreachable");
    expect(parsed.toolCallId).toBe("tc-abc-1");
    expect(parsed.toolName).toBe("code_exec");
    expect(parsed.agentName).toBe("Opex");
    expect(parsed.parallelBatchId).toBe("00000000-0000-0000-0000-000000000000");
  });

  test("tool-input-available preserves toolName and unknown input", () => {
    const raw = readFileSync(join(FIXTURES, "tool-input-available.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    expect(parsed.type).toBe("tool-input-available");
    if (parsed.type !== "tool-input-available") throw new Error("unreachable");
    expect(parsed.toolName).toBe("code_exec");
    const input = parsed.input as Record<string, unknown>;
    expect(input.cmd).toBe("ls");
  });

  test("tool-output-available output is a string", () => {
    const raw = readFileSync(join(FIXTURES, "tool-output-available.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "tool-output-available") throw new Error("unreachable");
    expect(typeof parsed.output).toBe("string");
    expect(parsed.output).toBe("file1.txt\nfile2.txt");
  });

  test("rich-card table variant has cardType=table and typed fields", () => {
    const raw = readFileSync(join(FIXTURES, "rich-card-table.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "rich-card") throw new Error("unreachable");
    expect(parsed.cardType).toBe("table");
    if (parsed.cardType !== "table") throw new Error("unreachable");
    expect(parsed.data.columns).toEqual(["id", "name"]);
    expect(parsed.data.rows).toHaveLength(2);
  });

  test("rich-card metric variant with trend", () => {
    const raw = readFileSync(join(FIXTURES, "rich-card-metric.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "rich-card") throw new Error("unreachable");
    if (parsed.cardType !== "metric") throw new Error("unreachable");
    expect(parsed.data.title).toBe("Latency");
    expect(parsed.data.trend).toBe("down");
  });

  test("rich-card other (fallback) preserves unknown cardType", () => {
    const raw = readFileSync(join(FIXTURES, "rich-card-other.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "rich-card") throw new Error("unreachable");
    if (parsed.cardType !== "other") throw new Error("unreachable");
    expect(parsed.data.cardType).toBe("experimental_chart");
  });

  test("tool-approval-needed has timeoutMs as number", () => {
    const raw = readFileSync(join(FIXTURES, "tool-approval-needed.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "tool-approval-needed") throw new Error("unreachable");
    expect(typeof parsed.timeoutMs).toBe("number");
    expect(parsed.timeoutMs).toBe(300000);
  });

  test("tool-approval-resolved action is a recognized string", () => {
    const raw = readFileSync(join(FIXTURES, "tool-approval-resolved.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "tool-approval-resolved") throw new Error("unreachable");
    expect(["approved", "rejected", "timeout_rejected"]).toContain(parsed.action);
  });

  test("reconnecting preserves snake_case delay_ms", () => {
    const raw = readFileSync(join(FIXTURES, "reconnecting.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "reconnecting") throw new Error("unreachable");
    expect(parsed.attempt).toBe(2);
    expect(parsed.delay_ms).toBe(1500);
  });

  test("usage with extended fields", () => {
    const raw = readFileSync(join(FIXTURES, "usage.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "usage") throw new Error("unreachable");
    expect(parsed.inputTokens).toBe(100);
    expect(parsed.outputTokens).toBe(50);
    expect(parsed.agentName).toBe("Opex");
    expect(parsed.cacheReadTokens).toBe(20);
    expect(parsed.cacheCreationTokens).toBe(5);
    expect(parsed.reasoningTokens).toBe(3);
  });

  test("sync (finished) preserves status + tool_calls", () => {
    const raw = readFileSync(join(FIXTURES, "sync-finished.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "sync") throw new Error("unreachable");
    expect(parsed.status).toBe("finished");
    expect(parsed.content).toBe("Final assistant response.");
    expect(parsed.toolCalls).toHaveLength(1);
  });

  test("sync (interrupted) emits error message", () => {
    const raw = readFileSync(join(FIXTURES, "sync-interrupted.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "sync") throw new Error("unreachable");
    expect(parsed.status).toBe("interrupted");
    expect(parsed.error).toBe("stream lost: core restarted");
  });

  test("sync-begin has boundaryMessageId + runStatus + truncated", () => {
    const raw = readFileSync(join(FIXTURES, "sync-begin.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    expect(parsed.type).toBe("sync_begin");
    if (parsed.type !== "sync_begin") throw new Error("unreachable");
    expect(parsed.boundaryMessageId).toBe(
      "00000000-0000-0000-0000-000000000000"
    );
    expect(parsed.runStatus).toBe("running");
    expect(parsed.truncated).toBe(false);
  });

  test("sync-begin (empty) has null boundaryMessageId + finished status", () => {
    const raw = readFileSync(join(FIXTURES, "sync-begin-empty.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "sync_begin") throw new Error("unreachable");
    expect(parsed.boundaryMessageId).toBe(null);
    expect(parsed.runStatus).toBe("finished");
  });

  test("sync-end has numeric lastSeq", () => {
    const raw = readFileSync(join(FIXTURES, "sync-end.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    expect(parsed.type).toBe("sync_end");
    if (parsed.type !== "sync_end") throw new Error("unreachable");
    expect(parsed.lastSeq).toBe(41);
  });

  test("sync-end (empty) has null lastSeq", () => {
    const raw = readFileSync(join(FIXTURES, "sync-end-empty.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "sync_end") throw new Error("unreachable");
    expect(parsed.lastSeq).toBe(null);
  });

  test("start fixture has messageId + agentName", () => {
    const raw = readFileSync(join(FIXTURES, "start.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "start") throw new Error("unreachable");
    expect(parsed.messageId).toBeDefined();
    expect(parsed.agentName).toBe("Opex");
  });

  test("step-start fixture has stepId + messageId + agentName", () => {
    const raw = readFileSync(join(FIXTURES, "step-start.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "step-start") throw new Error("unreachable");
    expect(parsed.stepId).toBe("step_2");
    expect(parsed.agentName).toBe("Opex");
  });

  test("text-start fixture has id + agentName", () => {
    const raw = readFileSync(join(FIXTURES, "text-start.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "text-start") throw new Error("unreachable");
    expect(parsed.id).toBe("text-1");
    expect(parsed.agentName).toBe("Opex");
  });

  test("text-end fixture has id", () => {
    const raw = readFileSync(join(FIXTURES, "text-end.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "text-end") throw new Error("unreachable");
    expect(parsed.id).toBe("text-1");
  });

  test("file fixture has url + mediaType (required)", () => {
    const raw = readFileSync(join(FIXTURES, "file.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "file") throw new Error("unreachable");
    expect(parsed.url).toBe("/uploads/x.png");
    expect(parsed.mediaType).toBe("image/png");
  });

  test("tool-input-delta fixture has toolCallId + inputTextDelta", () => {
    const raw = readFileSync(join(FIXTURES, "tool-input-delta.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "tool-input-delta") throw new Error("unreachable");
    expect(parsed.toolCallId).toBe("tc-abc-1");
    expect(parsed.inputTextDelta).toBe('{"cmd": "ls"}');
  });

  test("finish fixture has agentName", () => {
    const raw = readFileSync(join(FIXTURES, "finish.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "finish") throw new Error("unreachable");
    expect(parsed.agentName).toBe("Opex");
  });

  test("error fixture has errorText", () => {
    const raw = readFileSync(join(FIXTURES, "error.json"), "utf-8");
    const parsed = JSON.parse(raw) as SseEvent;
    if (parsed.type !== "error") throw new Error("unreachable");
    expect(parsed.errorText).toBe("Provider timeout");
  });

  test("all 30 fixtures present and round-trip cleanly", () => {
    const expected = new Set([
      "data-session-id.json",
      "start.json",
      "step-start.json",
      "text-start.json",
      "text-delta.json",
      "text-end.json",
      "tool-input-start.json",
      "tool-input-delta.json",
      "tool-input-available.json",
      "tool-output-available.json",
      "file.json",
      "rich-card-table.json",
      "rich-card-metric.json",
      "rich-card-metric-up.json",
      "rich-card-metric-flat.json",
      "rich-card-other.json",
      "tool-approval-needed.json",
      "tool-approval-resolved.json",
      "finish.json",
      "error.json",
      "reconnecting.json",
      "sync-finished.json",
      "sync-interrupted.json",
      "sync-error.json",
      "sync-running.json",
      "sync-begin.json",
      "sync-begin-empty.json",
      "sync-end.json",
      "sync-end-empty.json",
      "usage.json",
    ]);
    const actual = new Set(
      readdirSync(FIXTURES).filter((f) => f.endsWith(".json"))
    );
    expect(actual).toEqual(expected);

    // Round-trip stability for every fixture:
    for (const f of actual) {
      const raw = readFileSync(join(FIXTURES, f), "utf-8");
      const parsed = JSON.parse(raw) as SseEvent;
      expect(typeof parsed.type).toBe("string");
      const reSerialized = JSON.stringify(parsed);
      const reParsed = JSON.parse(reSerialized);
      expect(reParsed).toEqual(parsed);
    }
  });
});
