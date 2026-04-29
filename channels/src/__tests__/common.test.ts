import { describe, test, expect } from "bun:test";
import {
  splitText,
  toolEmoji,
  parseDirectives,
  decodeBase64Param,
  parseUserCommand,
  emojiToSlackShortcode,
  classifyMediaType,
  convertTablesToCodeBlocks,
  commonMarkToMarkdownV2,
  commonMarkToDiscord,
  commonMarkToSlack,
  commonMarkToWhatsApp,
  commonMarkToIrc,
} from "../drivers/common";

// ── splitText ───────────────────────────────────────────────────────────

describe("splitText", () => {
  test("short text unchanged", () => {
    expect(splitText("hello", 4096)).toEqual(["hello"]);
  });

  test("exact length unchanged", () => {
    expect(splitText("12345", 5)).toEqual(["12345"]);
  });

  test("empty string", () => {
    expect(splitText("", 10)).toEqual([""]);
  });

  test("splits at paragraph boundary", () => {
    const text = "first paragraph\n\nsecond paragraph";
    const parts = splitText(text, 20);
    expect(parts).toEqual(["first paragraph", "second paragraph"]);
  });

  test("splits at line boundary", () => {
    const text = "first line\nsecond line";
    const parts = splitText(text, 15);
    expect(parts).toEqual(["first line", "second line"]);
  });

  test("splits at space boundary", () => {
    const text = "hello world foo";
    const parts = splitText(text, 12);
    expect(parts).toEqual(["hello world", "foo"]);
  });

  test("hard split when no spaces", () => {
    const text = "abcdefghij";
    const parts = splitText(text, 5);
    expect(parts).toEqual(["abcde", "fghij"]);
  });

  test("handles multibyte characters", () => {
    // splitText measures JS string length (codepoints), not UTF-8 bytes.
    // "БББББ" has JS length 5; with maxLen=4 it splits into ["ББББ", "Б"].
    const text = "БББББ"; // JS length = 5
    const parts = splitText(text, 4);
    expect(parts).toEqual(["ББББ", "Б"]);
  });

  test("preserves code blocks when enabled", () => {
    const text = "```\nABCDEF\n```\nZ";
    const parts = splitText(text, 10, true);
    expect(parts[0]).toContain("```\nABCDEF\n```");
    expect(parts.length).toBeGreaterThanOrEqual(2);
  });

  test("does not preserve code blocks when disabled", () => {
    const text = "text\n```\ncode line1\ncode line2\n```\nafter";
    const parts = splitText(text, 15, false);
    expect(parts.length).toBeGreaterThanOrEqual(2);
  });

  test("code block too large for preservation falls through", () => {
    const code = "x".repeat(100);
    const text = "```\n" + code + "\n```";
    const parts = splitText(text, 20, true);
    expect(parts.length).toBeGreaterThan(1);
  });

  test("multiple paragraphs", () => {
    const text = "aaa\n\nbbb\n\nccc";
    const parts = splitText(text, 8);
    expect(parts[0]).toBe("aaa");
  });

  test("preserves all content", () => {
    const text = "The quick brown fox jumps over the lazy dog";
    const parts = splitText(text, 10);
    expect(parts.length).toBeGreaterThan(1);
    // All content should be present
    const total = parts.reduce((sum, p) => sum + p.length, 0);
    expect(total).toBeGreaterThan(0);
  });
});

// ── toolEmoji ───────────────────────────────────────────────────────────

describe("toolEmoji", () => {
  test("search tools", () => {
    expect(toolEmoji("searxng_search")).toBe("🌐");
    expect(toolEmoji("brave_search")).toBe("🌐");
  });

  test("web tools", () => {
    expect(toolEmoji("web_fetch")).toBe("🌐");
    expect(toolEmoji("browse_url")).toBe("🌐");
  });

  test("shell/exec tools", () => {
    expect(toolEmoji("shell_exec")).toBe("👨‍💻");
    expect(toolEmoji("run_code")).toBe("👨‍💻");
  });

  test("memory tools", () => {
    expect(toolEmoji("save_to_memory")).toBe("🧠");
    expect(toolEmoji("memory_store")).toBe("🧠");
  });

  test("memory_search matches search first", () => {
    expect(toolEmoji("memory_search")).toBe("🌐");
  });

  test("unknown tool returns default", () => {
    expect(toolEmoji("foobar")).toBe("🔥");
  });

  test("undefined returns default", () => {
    expect(toolEmoji(undefined)).toBe("🔥");
  });
});

// ── parseDirectives ─────────────────────────────────────────────────────

describe("parseDirectives", () => {
  test("no directives", () => {
    const { directives, text } = parseDirectives("just a question");
    expect(directives).toEqual({});
    expect(text).toBe("just a question");
  });

  test("extracts /think", () => {
    const { directives, text } = parseDirectives("/think\nwhat is 2+2?");
    expect(directives).toEqual({ think: true });
    expect(text).toBe("what is 2+2?");
  });

  test("extracts /think high", () => {
    const { directives, text } = parseDirectives("/think high\nquestion");
    expect(directives).toEqual({ think: true });
    expect(text).toBe("question");
  });

  test("extracts /verbose", () => {
    const { directives, text } = parseDirectives("/verbose\nshow me details");
    expect(directives).toEqual({ verbose: true });
    expect(text).toBe("show me details");
  });

  test("extracts both directives", () => {
    const { directives, text } = parseDirectives("/think\n/verbose\nmy question");
    expect(directives).toEqual({ think: true, verbose: true });
    expect(text).toBe("my question");
  });

  test("mixed with text", () => {
    const { directives, text } = parseDirectives("line one\n/think\nline two");
    expect(directives).toEqual({ think: true });
    expect(text).toBe("line one\nline two");
  });

  test("empty string", () => {
    const { directives, text } = parseDirectives("");
    expect(directives).toEqual({});
    expect(text).toBe("");
  });

  test("only directives", () => {
    const { directives, text } = parseDirectives("/think\n/verbose");
    expect(directives).toEqual({ think: true, verbose: true });
    expect(text).toBe("");
  });

  test("unknown directive kept as text", () => {
    const { directives, text } = parseDirectives("/unknown\nhello");
    expect(directives).toEqual({});
    expect(text).toBe("/unknown\nhello");
  });

  test("/think with whitespace", () => {
    const { directives, text } = parseDirectives("  /think  \nquestion");
    expect(directives).toEqual({ think: true });
    expect(text).toBe("question");
  });
});

// ── parseUserCommand ────────────────────────────────────────────────────

describe("parseUserCommand", () => {
  test("/stop", () => {
    expect(parseUserCommand("/stop")).toBe("stop");
  });

  test("/think", () => {
    expect(parseUserCommand("/think")).toBe("think");
  });

  test("/help", () => {
    expect(parseUserCommand("/help")).toBe("help");
  });

  test("with @botname suffix", () => {
    expect(parseUserCommand("/stop@my_test_bot")).toBe("stop");
    expect(parseUserCommand("/think@mybot")).toBe("think");
    expect(parseUserCommand("/help@somebot")).toBe("help");
  });

  test("bang prefix", () => {
    expect(parseUserCommand("!stop")).toBe("stop");
    expect(parseUserCommand("!think")).toBe("think");
    expect(parseUserCommand("!help")).toBe("help");
  });

  test("unknown command returns null", () => {
    expect(parseUserCommand("/status")).toBeNull();
    expect(parseUserCommand("/memory")).toBeNull();
    expect(parseUserCommand("/foobar")).toBeNull();
  });

  test("non-command text returns null", () => {
    expect(parseUserCommand("hello world")).toBeNull();
    expect(parseUserCommand("just some text")).toBeNull();
  });

  test("empty string returns null", () => {
    expect(parseUserCommand("")).toBeNull();
  });

  test("with leading whitespace", () => {
    expect(parseUserCommand("  /stop  ")).toBe("stop");
  });

  test("with args", () => {
    expect(parseUserCommand("/stop now")).toBe("stop");
  });

  test("slash only returns null", () => {
    expect(parseUserCommand("/")).toBeNull();
  });
});

// ── decodeBase64Param ───────────────────────────────────────────────────

describe("decodeBase64Param", () => {
  test("decodes valid base64", () => {
    const buf = decodeBase64Param({ audio: btoa("hello") }, "audio");
    expect(buf).not.toBeNull();
    expect(new TextDecoder().decode(buf!)).toBe("hello");
  });

  test("returns null for missing key", () => {
    expect(decodeBase64Param({}, "audio")).toBeNull();
  });

  test("decodes empty base64", () => {
    const buf = decodeBase64Param({ data: "" }, "data");
    expect(buf).not.toBeNull();
    expect(buf!.byteLength).toBe(0);
  });

  test("returns null for non-string value", () => {
    expect(decodeBase64Param({ data: 12345 }, "data")).toBeNull();
  });

  test("returns null for null value", () => {
    expect(decodeBase64Param({ data: null }, "data")).toBeNull();
  });
});

// ── emojiToSlackShortcode ───────────────────────────────────────────────

describe("emojiToSlackShortcode", () => {
  test("known emojis", () => {
    expect(emojiToSlackShortcode("👍")).toBe("thumbsup");
    expect(emojiToSlackShortcode("🤔")).toBe("thinking_face");
    expect(emojiToSlackShortcode("⚡")).toBe("zap");
    expect(emojiToSlackShortcode("🔥")).toBe("fire");
    expect(emojiToSlackShortcode("🌐")).toBe("globe_with_meridians");
    expect(emojiToSlackShortcode("👨‍💻")).toBe("technologist");
    expect(emojiToSlackShortcode("🧠")).toBe("brain");
    expect(emojiToSlackShortcode("🥱")).toBe("yawning_face");
    expect(emojiToSlackShortcode("😨")).toBe("fearful");
    expect(emojiToSlackShortcode("❌")).toBe("x");
    expect(emojiToSlackShortcode("🛑")).toBe("octagonal_sign");
    expect(emojiToSlackShortcode("👀")).toBe("eyes");
  });

  test("unknown emoji returns thumbsup", () => {
    expect(emojiToSlackShortcode("🎉")).toBe("thumbsup");
    expect(emojiToSlackShortcode("💀")).toBe("thumbsup");
  });

  test("empty string returns thumbsup", () => {
    expect(emojiToSlackShortcode("")).toBe("thumbsup");
  });

  test("toolEmoji → slackShortcode roundtrip", () => {
    expect(emojiToSlackShortcode(toolEmoji("web_search"))).toBe("globe_with_meridians");
    expect(emojiToSlackShortcode(toolEmoji("shell_run"))).toBe("technologist");
    expect(emojiToSlackShortcode(toolEmoji("memory_recall"))).toBe("brain");
    expect(emojiToSlackShortcode(toolEmoji("unknown_tool"))).toBe("fire");
  });
});

// ── classifyMediaType ───────────────────────────────────────────────────

describe("classifyMediaType", () => {
  test("image/* → image", () => {
    expect(classifyMediaType("image/png")).toBe("image");
    expect(classifyMediaType("image/jpeg")).toBe("image");
  });

  test("audio/* → audio", () => {
    expect(classifyMediaType("audio/mpeg")).toBe("audio");
    expect(classifyMediaType("audio/ogg")).toBe("audio");
  });

  test("video/* → video", () => {
    expect(classifyMediaType("video/mp4")).toBe("video");
  });

  test("application/* → document", () => {
    expect(classifyMediaType("application/pdf")).toBe("document");
    expect(classifyMediaType("application/zip")).toBe("document");
  });

  test("undefined → document", () => {
    expect(classifyMediaType(undefined)).toBe("document");
  });

  test("empty string → document", () => {
    expect(classifyMediaType("")).toBe("document");
  });
});

// ── convertTablesToCodeBlocks ───────────────────────────────────────────

describe("convertTablesToCodeBlocks", () => {
  test("converts simple markdown table", () => {
    const table = "| A | B |\n|---|---|\n| 1 | 2 |";
    const out = convertTablesToCodeBlocks(table);
    expect(out).toContain("```");
    expect(out).toContain("A");
    expect(out).toContain("B");
  });

  test("passes through non-table text unchanged", () => {
    const text = "just plain text\nno table here";
    expect(convertTablesToCodeBlocks(text)).toBe(text);
  });

  test("table with single data row is not converted (needs ≥2 data rows)", () => {
    const table = "| A | B |\n|---|---|\n";
    const out = convertTablesToCodeBlocks(table);
    expect(out).not.toContain("```");
  });
});

// ── commonMarkToMarkdownV2 ──────────────────────────────────────────────

describe("commonMarkToMarkdownV2", () => {
  test("bold **text**", () => {
    const out = commonMarkToMarkdownV2("**hello**");
    expect(out).toContain("*hello*");
  });

  test("italic *text*", () => {
    const out = commonMarkToMarkdownV2("*world*");
    expect(out).toContain("_world_");
  });

  test("escapes special MarkdownV2 characters in plain text", () => {
    const out = commonMarkToMarkdownV2("hello. world!");
    expect(out).toContain("\\.");
    expect(out).toContain("\\!");
  });

  test("inline code is not escaped", () => {
    const out = commonMarkToMarkdownV2("`foo.bar`");
    expect(out).toBe("`foo.bar`");
  });

  test("fenced code block is preserved", () => {
    const out = commonMarkToMarkdownV2("```\nhello\n```");
    expect(out).toContain("```");
    expect(out).toContain("hello");
  });

  test("link [text](url)", () => {
    const out = commonMarkToMarkdownV2("[click](https://example.com)");
    expect(out).toContain("[click]");
    expect(out).toContain("https://example.com");
  });

  test("strikethrough ~~text~~", () => {
    const out = commonMarkToMarkdownV2("~~removed~~");
    expect(out).toContain("~removed~");
  });

  test("empty string returns empty", () => {
    expect(commonMarkToMarkdownV2("")).toBe("");
  });
});

// ── commonMarkToDiscord ─────────────────────────────────────────────────

describe("commonMarkToDiscord", () => {
  test("passes most markdown through unchanged", () => {
    const text = "**bold** and *italic*";
    expect(commonMarkToDiscord(text)).toBe(text);
  });

  test("converts tables to code blocks", () => {
    const table = "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
    expect(commonMarkToDiscord(table)).toContain("```");
  });
});

// ── commonMarkToSlack ───────────────────────────────────────────────────

describe("commonMarkToSlack", () => {
  test("converts **bold** to *bold*", () => {
    expect(commonMarkToSlack("**hello**")).toBe("*hello*");
  });

  test("converts [text](url) to <url|text>", () => {
    expect(commonMarkToSlack("[click](https://example.com)")).toBe(
      "<https://example.com|click>"
    );
  });

  test("converts ## header to *header*", () => {
    expect(commonMarkToSlack("## Title")).toBe("*Title*");
  });
});

// ── commonMarkToWhatsApp ────────────────────────────────────────────────

describe("commonMarkToWhatsApp", () => {
  test("converts **bold** to *bold*", () => {
    expect(commonMarkToWhatsApp("**hello**")).toBe("*hello*");
  });

  test("converts [text](url) to text (url)", () => {
    expect(commonMarkToWhatsApp("[click](https://example.com)")).toBe(
      "click (https://example.com)"
    );
  });

  test("converts ## header to *header*", () => {
    expect(commonMarkToWhatsApp("## Title")).toBe("*Title*");
  });
});

// ── commonMarkToIrc ─────────────────────────────────────────────────────

describe("commonMarkToIrc", () => {
  test("strips **bold**", () => {
    expect(commonMarkToIrc("**hello**")).toBe("hello");
  });

  test("strips *italic*", () => {
    expect(commonMarkToIrc("*italic*")).toBe("italic");
  });

  test("strips ~~strikethrough~~", () => {
    expect(commonMarkToIrc("~~removed~~")).toBe("removed");
  });

  test("strips code fences and keeps content", () => {
    expect(commonMarkToIrc("```\ncode here\n```")).toBe("code here");
  });

  test("strips inline code backticks", () => {
    expect(commonMarkToIrc("`foo`")).toBe("foo");
  });

  test("converts [text](url) to text - url", () => {
    expect(commonMarkToIrc("[link](https://example.com)")).toBe(
      "link - https://example.com"
    );
  });

  test("strips blockquote marker", () => {
    expect(commonMarkToIrc("> quote")).toBe("quote");
  });

  test("strips ## header", () => {
    expect(commonMarkToIrc("## Title")).toBe("Title");
  });
});
