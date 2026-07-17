/**
 * Shared utilities for all channel drivers.
 * Port of crates/opex-channel/src/channels/common.rs
 */

// ── splitText ───────────────────────────────────────────────────────────

/**
 * Split text into chunks respecting maxLen, preferring paragraph / line / space boundaries.
 * If preserveCodeBlocks is true, tries to keep ``` blocks together (up to 2× maxLen).
 */
export function splitText(
  text: string,
  maxLen: number,
  preserveCodeBlocks = false,
): string[] {
  if (text.length <= maxLen) {
    return [text];
  }

  const chunks: string[] = [];
  let remaining = text;

  while (remaining.length > 0) {
    if (remaining.length <= maxLen) {
      chunks.push(remaining);
      break;
    }

    const window = remaining.slice(0, maxLen);

    // If preserving code blocks, avoid splitting inside a fenced block.
    if (preserveCodeBlocks) {
      const openCount = (window.match(/```/g) || []).length;
      if (openCount % 2 === 1) {
        const afterWindow = remaining.slice(maxLen);
        const closePos = afterWindow.indexOf("```");
        if (closePos !== -1) {
          const end = maxLen + closePos + 3;
          if (end <= maxLen * 2) {
            chunks.push(remaining.slice(0, end));
            remaining = remaining.slice(end).replace(/^\n/, "");
            continue;
          }
        }
      }
    }

    // Prefer splitting at paragraph boundary (double newline).
    const paraPos = window.lastIndexOf("\n\n");
    if (paraPos !== -1) {
      const splitAt = paraPos + 2;
      chunks.push(remaining.slice(0, splitAt).trimEnd());
      remaining = remaining.slice(splitAt);
      continue;
    }

    // Then at a single newline.
    const linePos = window.lastIndexOf("\n");
    if (linePos !== -1) {
      const splitAt = linePos + 1;
      chunks.push(remaining.slice(0, splitAt).trimEnd());
      remaining = remaining.slice(splitAt);
      continue;
    }

    // Then at a space.
    const spacePos = window.lastIndexOf(" ");
    if (spacePos !== -1) {
      const splitAt = spacePos + 1;
      chunks.push(remaining.slice(0, splitAt).trimEnd());
      remaining = remaining.slice(splitAt);
      continue;
    }

    // Hard split as a last resort.
    chunks.push(remaining.slice(0, maxLen));
    remaining = remaining.slice(maxLen);
  }

  return chunks;
}

// ── toolEmoji ───────────────────────────────────────────────────────────

/** Return an emoji representing the kind of tool being invoked. */
export function toolEmoji(toolName: string | undefined): string {
  if (!toolName) return "\u{1F525}";
  if (
    toolName.includes("search") ||
    toolName.includes("web") ||
    toolName.includes("fetch") ||
    toolName.includes("browse")
  ) {
    return "\u{1F310}";
  }
  if (
    toolName.includes("shell") ||
    toolName.includes("exec") ||
    toolName.includes("code")
  ) {
    return "\u{1F468}\u200D\u{1F4BB}";
  }
  if (toolName.includes("memory")) return "\u{1F9E0}";
  return "\u{1F525}";
}

// ── parseDirectives ─────────────────────────────────────────────────────

/** Strip directive lines (/think, /verbose) from user text. */
export function parseDirectives(text: string): {
  text: string;
  directives: Record<string, boolean>;
} {
  const directives: Record<string, boolean> = {};
  const cleanLines: string[] = [];

  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (trimmed === "/think" || trimmed === "/think high") {
      directives.think = true;
    } else if (trimmed === "/verbose") {
      directives.verbose = true;
    } else {
      cleanLines.push(line);
    }
  }

  return {
    text: cleanLines.join("\n").trim(),
    directives,
  };
}

// ── parseUserCommand ────────────────────────────────────────────────────

export type UserCommand = "stop" | "think" | "help";

/**
 * Parse /stop, /think, /help commands.
 * Returns null for unrecognised commands (they fall through to core).
 */
export function parseUserCommand(text: string): UserCommand | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/") && !trimmed.startsWith("!")) {
    return null;
  }

  // Strip @botname (e.g. "/stop@my_bot" → "/stop")
  const [cmdWithArgs] = trimmed.split(" ", 1);
  const cmd = cmdWithArgs.split("@")[0];

  switch (cmd) {
    case "/stop":
    case "!stop":
      return "stop";
    case "/think":
    case "!think":
      return "think";
    case "/help":
    case "!help":
      return "help";
    default:
      return null;
  }
}

// ── classifyMediaType ───────────────────────────────────────────────────

/** Classify MIME type into media category. */
export function classifyMediaType(mimeType: string | undefined): import("../types").MediaType {
  if (!mimeType) return "document";
  if (mimeType.startsWith("image")) return "image";
  if (mimeType.startsWith("audio")) return "audio";
  if (mimeType.startsWith("video")) return "video";
  return "document";
}

// ── reUploadAttachments ─────────────────────────────────────────────────

/** Re-upload attachments through core for stable URLs. */
export async function reUploadAttachments(
  bridge: { uploadMedia: (url: string, filename: string, authHeader?: string) => Promise<string> },
  attachments: import("../types").MediaAttachment[],
  authHeader?: string,
): Promise<import("../types").MediaAttachment[]> {
  return Promise.all(
    attachments.map(async (att) => {
      const localUrl = await bridge.uploadMedia(att.url, att.file_name ?? "file", authHeader);
      return { ...att, url: localUrl };
    }),
  );
}

// ── commonMarkToMarkdownV2 ──────────────────────────────────────────────

/**
 * MarkdownV2 special characters that must be escaped in plain text.
 * See: https://core.telegram.org/bots/api#markdownv2-style
 */
const MD_V2_SPECIAL = /[_*\[\]()~`>#+\-=|{}.!\\]/g;

/** Escape all MarkdownV2 special characters in plain text. */
function escapeMarkdownV2(text: string): string {
  return text.replace(MD_V2_SPECIAL, "\\$&");
}

/** Inside ``` and ` blocks, only ` and \ need escaping. */
function escapeCodeContent(text: string): string {
  return text.replace(/[`\\]/g, "\\$&");
}

/** Inside link URLs, only ) and \ need escaping. */
function escapeUrlContent(text: string): string {
  return text.replace(/[)\\]/g, "\\$&");
}

// Sentinel characters for formatting markers (control chars, won't match \w)
const S_BOLD_O = "\x01"; // bold open
const S_BOLD_C = "\x02"; // bold close
const S_ITAL_O = "\x03"; // italic open
const S_ITAL_C = "\x04"; // italic close
const S_STRK_O = "\x05"; // strikethrough open
const S_STRK_C = "\x06"; // strikethrough close
const S_BQUOT = "\x07";  // blockquote line marker

/**
 * Convert CommonMark-ish LLM output to Telegram MarkdownV2.
 *
 * Strategy:
 *  1. Extract protected regions (code blocks, inline code, links)
 *  2. Convert CommonMark markers to sentinel placeholders
 *  3. Escape MarkdownV2 specials in plain text (sentinels stay)
 *  4. Replace sentinels with MarkdownV2 formatting characters
 *  5. Restore protected regions
 */
export function commonMarkToMarkdownV2(text: string): string {
  // ── Step 0: Convert markdown tables to code blocks ────────────────
  const preprocessed = convertTablesToCodeBlocks(text);

  // ── Step 1: Extract fenced code blocks ────────────────────────────
  const codeBlocks: string[] = [];
  let result = preprocessed.replace(/```(\w*)\n([\s\S]*?)```/g, (_m, lang, code) => {
    const idx = codeBlocks.length;
    const escaped = escapeCodeContent(code.replace(/\n$/, ""));
    codeBlocks.push("```" + (lang || "") + "\n" + escaped + "```");
    return `\x00CB${idx}\x00`;
  });

  // ── Step 2: Extract inline code ───────────────────────────────────
  const inlineCodes: string[] = [];
  result = result.replace(/`([^`\n]+)`/g, (_m, code) => {
    const idx = inlineCodes.length;
    inlineCodes.push("`" + escapeCodeContent(code) + "`");
    return `\x00IC${idx}\x00`;
  });

  // ── Step 3: Extract links [text](url) ─────────────────────────────
  const links: string[] = [];
  result = result.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, linkText, url) => {
    const idx = links.length;
    links.push(linkText + "\x00LS" + url);
    return `\x00LK${idx}\x00`;
  });

  // ── Step 4: Convert CommonMark formatting → sentinels ─────────────
  // Bold+italic: ***text*** (must be before bold)
  result = result.replace(
    /\*\*\*(.+?)\*\*\*/g,
    `${S_BOLD_O}${S_ITAL_O}$1${S_ITAL_C}${S_BOLD_C}`,
  );
  // Bold: **text**
  result = result.replace(/\*\*(.+?)\*\*/g, `${S_BOLD_O}$1${S_BOLD_C}`);
  // Bold: __text__ (CommonMark bold)
  result = result.replace(/__(.+?)__/g, `${S_BOLD_O}$1${S_BOLD_C}`);
  // Italic: *text*
  result = result.replace(
    /(?<!\w)\*([^\s*](?:.*?[^\s*])?)\*(?!\w)/g,
    `${S_ITAL_O}$1${S_ITAL_C}`,
  );
  // Italic: _text_
  result = result.replace(
    /(?<!\w)_([^\s_](?:.*?[^\s_])?)_(?!\w)/g,
    `${S_ITAL_O}$1${S_ITAL_C}`,
  );
  // Strikethrough: ~~text~~ → ~text~
  result = result.replace(/~~(.+?)~~/g, `${S_STRK_O}$1${S_STRK_C}`);

  // Headers: # text → bold
  result = result.replace(/^#{1,6}\s+(.+)$/gm, `${S_BOLD_O}$1${S_BOLD_C}`);
  // Horizontal rules → Unicode line
  result = result.replace(/^[-]{3,}\s*$/gm, "\u2014\u2014\u2014");
  result = result.replace(/^[=]{3,}\s*$/gm, "\u2550\u2550\u2550");
  result = result.replace(/^[*_]{3,}\s*$/gm, "\u2014\u2014\u2014");
  // Unordered list items: - item or * item → bullet
  result = result.replace(/^(\s*)[-*]\s+/gm, "$1\u2022 ");
  // Blockquotes: > text
  result = result.replace(/^> ?(.+)$/gm, `${S_BQUOT}$1`);

  // ── Step 5: Escape MarkdownV2 specials in plain text ──────────────
  // Process character by character to skip sentinels and placeholders
  let escaped = "";
  let pos = 0;
  while (pos < result.length) {
    const ch = result.charCodeAt(pos);

    // Sentinel characters (\x00-\x07) — pass through
    if (ch <= 0x07) {
      // \x00 starts a placeholder like \x00CB0\x00 — find closing \x00
      if (ch === 0x00) {
        const end = result.indexOf("\x00", pos + 1);
        if (end !== -1) {
          escaped += result.slice(pos, end + 1);
          pos = end + 1;
        } else {
          escaped += result[pos];
          pos++;
        }
      } else {
        // \x01-\x07: formatting sentinel, pass through
        escaped += result[pos];
        pos++;
      }
      continue;
    }

    // Plain text character — escape if special
    const c = result[pos];
    if (MD_V2_SPECIAL.test(c)) {
      // Reset regex lastIndex since we use .test() on a global regex
      MD_V2_SPECIAL.lastIndex = 0;
      escaped += "\\" + c;
    } else {
      escaped += c;
    }
    pos++;
  }
  result = escaped;

  // ── Step 6: Replace sentinels → MarkdownV2 markers ────────────────
  result = result.replace(/\x01/g, "*");     // bold open
  result = result.replace(/\x02/g, "*");     // bold close
  result = result.replace(/\x03/g, "_");     // italic open
  result = result.replace(/\x04/g, "_");     // italic close
  result = result.replace(/\x05/g, "~");     // strikethrough open
  result = result.replace(/\x06/g, "~");     // strikethrough close
  result = result.replace(/\x07/g, ">");     // blockquote

  // ── Step 7: Restore links ────────────────────────────────────────
  result = result.replace(/\x00LK(\d+)\x00/g, (_m, idx) => {
    const raw = links[Number(idx)];
    const sepIdx = raw.indexOf("\x00LS");
    const linkText = raw.slice(0, sepIdx);
    const url = raw.slice(sepIdx + 3);
    return "[" + escapeMarkdownV2(linkText) + "](" + escapeUrlContent(url) + ")";
  });

  // ── Step 8: Restore code blocks and inline code ───────────────────
  result = result.replace(/\x00IC(\d+)\x00/g, (_m, idx) => inlineCodes[Number(idx)]);
  result = result.replace(/\x00CB(\d+)\x00/g, (_m, idx) => codeBlocks[Number(idx)]);

  return result;
}

// ── emojiToSlackShortcode ───────────────────────────────────────────────

const EMOJI_MAP: Record<string, string> = {
  "\u{1F44D}": "thumbsup",
  "\u{1F914}": "thinking_face",
  "\u26A1": "zap",
  "\u{1F525}": "fire",
  "\u{1F310}": "globe_with_meridians",
  "\u{1F468}\u200D\u{1F4BB}": "technologist",
  "\u{1F9E0}": "brain",
  "\u{1F971}": "yawning_face",
  "\u{1F628}": "fearful",
  "\u274C": "x",
  "\u{1F6D1}": "octagonal_sign",
  "\u{1F440}": "eyes",
};

/** Convert a Unicode emoji to a Slack-compatible shortcode (without colons). */
export function emojiToSlackShortcode(emoji: string): string {
  return EMOJI_MAP[emoji] ?? "thumbsup";
}

// ── Channel-specific text converters ──────────────────────────────────

/**
 * Convert markdown tables to aligned code blocks.
 * Shared by all channels that don't support native tables.
 */
export function convertTablesToCodeBlocks(text: string): string {
  const lines = text.split("\n");
  const out: string[] = [];
  let i = 0;
  while (i < lines.length) {
    const trimmed = lines[i].trim();
    if (trimmed.startsWith("|") && (trimmed.match(/\|/g) || []).length >= 3) {
      const tableLines: string[] = [];
      while (i < lines.length) {
        const t = lines[i].trim();
        if (t.startsWith("|") && (t.match(/\|/g) || []).length >= 2) {
          tableLines.push(t);
          i++;
        } else {
          break;
        }
      }
      const dataLines = tableLines.filter((l) => !/^\|[\s:|-]+\|$/.test(l));
      if (dataLines.length >= 2) {
        const rows = dataLines.map((l) =>
          l.replace(/^\||\|$/g, "").split("|").map((c) => c.trim())
        );
        const numCols = Math.max(...rows.map((r) => r.length));
        const widths = Array.from({ length: numCols }, (_, j) =>
          Math.max(...rows.map((r) => (r[j] || "").length), 1)
        );
        const aligned = rows.map((r) =>
          Array.from({ length: numCols }, (_, j) =>
            (r[j] || "").padEnd(widths[j])
          ).join("  ")
        );
        out.push("```", ...aligned, "```");
      } else {
        out.push(...tableLines);
      }
    } else {
      out.push(lines[i]);
      i++;
    }
  }
  return out.join("\n");
}

/**
 * Convert CommonMark to Discord-compatible markdown.
 * Discord supports most markdown but NOT tables.
 */
export function commonMarkToDiscord(text: string): string {
  return convertTablesToCodeBlocks(text);
}

/**
 * Convert CommonMark to Slack mrkdwn format.
 * Key differences: *bold* (single), <url|text> links, no tables.
 */
export function commonMarkToSlack(text: string): string {
  let result = convertTablesToCodeBlocks(text);
  // **bold** → *bold*
  result = result.replace(/\*\*(.+?)\*\*/g, "*$1*");
  // [text](url) → <url|text>
  result = result.replace(/\[([^\]]+)\]\(([^)]+)\)/g, "<$2|$1>");
  // ## headers → *bold* (Slack has no headers)
  result = result.replace(/^#{1,6}\s+(.+)$/gm, "*$1*");
  return result;
}

/**
 * Convert CommonMark to WhatsApp format.
 * Key differences: *bold* (single), _italic_ (underscore), no links, no tables.
 */
export function commonMarkToWhatsApp(text: string): string {
  let result = convertTablesToCodeBlocks(text);
  // **bold** → *bold*
  result = result.replace(/\*\*(.+?)\*\*/g, "*$1*");
  // [text](url) → text (url)
  result = result.replace(/\[([^\]]+)\]\(([^)]+)\)/g, "$1 ($2)");
  // ## headers → *bold*
  result = result.replace(/^#{1,6}\s+(.+)$/gm, "*$1*");
  return result;
}

/**
 * Strip ALL markdown for IRC (plain text only).
 */
export function commonMarkToIrc(text: string): string {
  let result = text;
  // Remove code blocks — keep content
  result = result.replace(/```\w*\n([\s\S]*?)```/g, "$1");
  // Remove inline code backticks
  result = result.replace(/`([^`]+)`/g, "$1");
  // **bold** → plain
  result = result.replace(/\*\*(.+?)\*\*/g, "$1");
  // *italic* → plain
  result = result.replace(/\*(.+?)\*/g, "$1");
  // _italic_ → plain
  result = result.replace(/_(.+?)_/g, "$1");
  // ~~strike~~ → plain
  result = result.replace(/~~(.+?)~~/g, "$1");
  // [text](url) → text - url
  result = result.replace(/\[([^\]]+)\]\(([^)]+)\)/g, "$1 - $2");
  // ## headers → plain
  result = result.replace(/^#{1,6}\s+(.+)$/gm, "$1");
  // > blockquotes → plain
  result = result.replace(/^>\s?/gm, "");
  // Tables → simple lines
  result = convertTablesToCodeBlocks(result);
  // Remove remaining code fences (from table conversion)
  result = result.replace(/^```$/gm, "");
  return result.trim();
}

// ── Telegram retry utilities ───────────────────────────────────────────

/** Exponential backoff: 1s → 2s → 4s → ... capped at 30s */
export function exponentialDelay(attempt: number): number {
  return Math.min(1000 * Math.pow(2, attempt), 30_000);
}

/** Build the per-chat cooldown map key. */
export function chatCooldownKey(chatId: number, threadId?: number): string {
  return `${chatId}:${threadId ?? ""}`;
}

// ── Error classification ───────────────────────────────────────────────

/**
 * Extract HTTP status code from a Telegram API error message.
 * Returns null if no code found.
 */
export function extractTgErrorCode(error: unknown): number | null {
  const msg = String(error);
  const match = msg.match(/\b(4\d{2}|5\d{2})\b/);
  return match ? parseInt(match[1], 10) : null;
}

/**
 * Extract retry_after seconds from a Telegram 429 error.
 * Telegram API returns: {"ok":false,"error_code":429,"parameters":{"retry_after":N}}
 */
export function extractTgRetryAfter(error: unknown): number | null {
  if (typeof error === "object" && error !== null) {
    const e = error as Record<string, unknown>;
    const retryAfter = (e["parameters"] as Record<string, unknown>)?.["retry_after"];
    if (typeof retryAfter === "number") return retryAfter;
  }
  // Fallback: parse from string
  const msg = String(error);
  const match = msg.match(/retry.after[:\s]+(\d+)/i);
  return match ? parseInt(match[1], 10) : null;
}

/** Returns true if a Telegram API error is permanent (should NOT be retried). */
export function isTgPermanentError(error: unknown): boolean {
  const msg = String(error);
  if (msg.includes("403") || msg.includes("Forbidden")) return true;
  if (msg.includes("401") || msg.includes("Unauthorized")) return true;
  if (
    msg.includes("400") &&
    (msg.includes("chat not found") ||
      msg.includes("CHAT_NOT_FOUND") ||
      msg.includes("USER_DEACTIVATED") ||
      msg.includes("bot was blocked") ||
      msg.includes("not enough rights"))
  )
    return true;
  return false;
}
