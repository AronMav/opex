import { describe, it, expect } from "vitest";
import { cleanContent, formatDuration, formatBytes } from "@/lib/format";

describe("cleanContent", () => {
  it("removes closed <think> blocks", () => {
    expect(cleanContent("Hello <think>reasoning</think> world")).toBe("Hello world");
  });

  it("removes unclosed <think> at end", () => {
    expect(cleanContent("Hello <think>still thinking")).toBe("Hello");
  });

  it("removes multiple <think> blocks", () => {
    expect(cleanContent("<think>a</think>text<think>b</think>more")).toBe("textmore");
  });

  it("removes minimax tool_call tags", () => {
    expect(cleanContent("Hello <minimax:tool_call>call</minimax:tool_call> world")).toBe("Hello world");
  });

  it("removes [TOOL_CALL] tags", () => {
    expect(cleanContent("Hello [TOOL_CALL]call[/TOOL_CALL] world")).toBe("Hello world");
  });

  it("returns empty for think-only content", () => {
    expect(cleanContent("<think>only reasoning</think>")).toBe("");
  });

  it("handles empty string", () => {
    expect(cleanContent("")).toBe("");
  });

  it("passes through normal text unchanged", () => {
    expect(cleanContent("Hello world")).toBe("Hello world");
  });

  it("trims surrounding whitespace", () => {
    expect(cleanContent("  hello  ")).toBe("hello");
  });

  it("returns empty for whitespace-only content", () => {
    expect(cleanContent("   ")).toBe("");
  });

  it("removes unclosed [TOOL_CALL] at end", () => {
    expect(cleanContent("text [TOOL_CALL]partial")).toBe("text");
  });
});

describe("formatDuration", () => {
  it("formats seconds", () => {
    expect(formatDuration(45)).toBe("45s");
  });

  it("formats minutes", () => {
    expect(formatDuration(120)).toBe("2m");
  });

  it("formats hours and minutes", () => {
    expect(formatDuration(3660)).toBe("1h 1m");
  });

  it("formats exact hours", () => {
    expect(formatDuration(7200)).toBe("2h");
  });

  it("handles zero", () => {
    expect(formatDuration(0)).toBe("0s");
  });

  it("formats exactly 59 seconds as seconds", () => {
    expect(formatDuration(59)).toBe("59s");
  });

  it("formats exactly 60 seconds as minutes", () => {
    expect(formatDuration(60)).toBe("1m");
  });

  it("formats exactly 3600 seconds as 1h (no minutes)", () => {
    expect(formatDuration(3600)).toBe("1h");
  });
});

describe("formatBytes", () => {
  it("formats bytes", () => {
    expect(formatBytes(500)).toBe("500 B");
  });

  it("formats kilobytes", () => {
    expect(formatBytes(2048)).toBe("2.0 KB");
  });

  it("formats megabytes", () => {
    expect(formatBytes(5242880)).toBe("5.0 MB");
  });

  it("formats gigabytes", () => {
    expect(formatBytes(1073741824)).toBe("1.0 GB");
  });

  it("handles zero", () => {
    expect(formatBytes(0)).toBe("0 B");
  });

  it("formats exactly 1023 bytes as B (below KB threshold)", () => {
    expect(formatBytes(1023)).toBe("1023 B");
  });

  it("formats exactly 1024 bytes as 1.0 KB", () => {
    expect(formatBytes(1024)).toBe("1.0 KB");
  });
});
