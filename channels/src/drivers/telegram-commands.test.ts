import { describe, it, expect } from "bun:test";
import { commandsToTelegram } from "./telegram-commands";

describe("commandsToTelegram", () => {
  it("maps name+description and keeps valid names", () => {
    const out = commandsToTelegram([
      { name: "status", description: "Show status" },
      { name: "summarize_video", description: "Summarize a video" },
    ]);
    expect(out).toEqual([
      { command: "status", description: "Show status" },
      { command: "summarize_video", description: "Summarize a video" },
    ]);
  });

  it("drops names Telegram rejects (uppercase, hyphen, >32, empty)", () => {
    const out = commandsToTelegram([
      { name: "Status", description: "x" },     // uppercase
      { name: "export-session", description: "x" }, // hyphen
      { name: "a".repeat(33), description: "x" },   // too long
      { name: "", description: "x" },
      { name: "ok_cmd", description: "y" },
    ]);
    expect(out).toEqual([{ command: "ok_cmd", description: "y" }]);
  });

  it("truncates description to 256 chars", () => {
    const out = commandsToTelegram([{ name: "x", description: "d".repeat(300) }]);
    expect(out[0].description.length).toBe(256);
  });

  it("falls back to the command name when description is empty or whitespace (Telegram rejects empty desc → whole batch fails)", () => {
    const out = commandsToTelegram([
      { name: "blank", description: "" },
      { name: "spaces", description: "   " },
    ]);
    expect(out).toEqual([
      { command: "blank", description: "blank" },
      { command: "spaces", description: "spaces" },
    ]);
  });
});
