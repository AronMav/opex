import type { ChatMessage } from "@/stores/chat-store";
import type { SessionRow } from "@/types/api";

const BCP47: Record<string, string> = { ru: "ru-RU", en: "en-US" };

export function formatDate(iso: string, locale = "en"): string {
  return new Date(iso).toLocaleDateString(BCP47[locale] || "en-US", {
    day: "numeric",
    month: "short",
    year: "numeric",
  });
}

const RELATIVE_TIME_STRINGS: Record<string, { now: string; yesterday: string }> = {
  ru: { now: "сейчас", yesterday: "вчера" },
  en: { now: "now", yesterday: "yesterday" },
};

export function relativeTime(ts: number | string, locale = "en"): string {
  const d = typeof ts === "string" ? new Date(ts) : new Date(ts);
  const diff = Date.now() - d.getTime();
  const strings = RELATIVE_TIME_STRINGS[locale] ?? RELATIVE_TIME_STRINGS.en;
  if (diff < 60_000) return strings.now;
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)}m`;
  if (diff < 86400_000) return `${Math.floor(diff / 3600_000)}h`;
  const yesterday = new Date(Date.now() - 86400_000);
  if (d.toDateString() === yesterday.toDateString()) return strings.yesterday;
  return d.toLocaleDateString(BCP47[locale] || "en-US", { day: "numeric", month: "short" });
}

export function formatMessageTime(iso: string, locale = "en"): string {
  const d = new Date(iso);
  const now = new Date();
  const hhmm = d.toLocaleTimeString(BCP47[locale] || "en-US", {
    hour: "2-digit",
    minute: "2-digit",
  });
  if (d.toDateString() === now.toDateString()) return hhmm;
  const dd = String(d.getDate()).padStart(2, "0");
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  return `${dd}.${mm} ${hhmm}`;
}

export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return m > 0 ? `${h}h ${m}m` : `${h}h`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1073741824) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${(bytes / 1073741824).toFixed(1)} GB`;
}

export function sessionToMarkdown(
  messages: ChatMessage[],
  session: SessionRow,
  agentName: string,
): string {
  const lines: string[] = [];

  lines.push(`# Session Export`);
  lines.push(``);
  lines.push(`**Agent:** ${agentName}  `);
  lines.push(`**Session:** \`${session.id}\`  `);
  lines.push(`**Date:** ${new Date(session.started_at).toLocaleString()}  `);
  lines.push(``);
  lines.push(`---`);
  lines.push(``);

  for (const msg of messages) {
    const role = msg.role === "user" ? "**You**" : `**${agentName}**`;
    lines.push(`### ${role}`);
    lines.push(``);

    for (const part of msg.parts) {
      if (part.type === "text") {
        lines.push(part.text ?? "");
      } else if (part.type === "reasoning") {
        lines.push(`> *${part.text ?? ""}*`);
      } else if (part.type === "tool") {
        lines.push(`\`[${part.toolName ?? "tool"}]\``);
        if (part.output != null) {
          const result =
            typeof part.output === "string"
              ? part.output
              : JSON.stringify(part.output, null, 2);
          lines.push(`\`\`\`\n${result.slice(0, 1000)}\n\`\`\``);
        }
      }
      lines.push(``);
    }

    lines.push(`---`);
    lines.push(``);
  }

  return lines.join("\n");
}

export function truncateOutput(
  text: string,
  max: number,
): { text: string; truncated: boolean; hiddenChars: number } {
  if (text.length <= max) return { text, truncated: false, hiddenChars: 0 };
  return { text: text.slice(0, max), truncated: true, hiddenChars: text.length - max };
}

export function cleanContent(text: string): string {
  let s = text;
  s = s.replace(/<think>[\s\S]*?<\/think>\s*/g, "");
  s = s.replace(/<think>[\s\S]*$/g, ""); // unclosed think block
  s = s.replace(/<minimax:tool_call>[\s\S]*?(<\/minimax:tool_call>|$)\s*/g, "");
  s = s.replace(/\[TOOL_CALL\][\s\S]*?(\[\/TOOL_CALL\]|$)\s*/g, "");
  return s.trim();
}
