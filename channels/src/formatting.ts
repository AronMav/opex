/**
 * Channel-specific formatting prompts for the LLM system prompt.
 *
 * Prompts live as standalone Markdown files in workspace/prompts/formatting/
 * (one per channel type) so they can be edited without rebuilding/restarting
 * the channels TypeScript service. Files are read once at module load time;
 * restart the channels process to pick up edits.
 *
 * Layout: <repo>/channels/src/formatting.ts loads from <repo>/workspace/prompts/formatting/<channel>.md
 *
 * Sent to core via the Ready WS message; injected only when channel is connected.
 */

import * as fs from "node:fs";
import * as path from "node:path";

const PROMPTS_DIR = path.resolve(import.meta.dir, "../../workspace/prompts/formatting");

const SUPPORTED_CHANNELS = [
  "telegram",
  "discord",
  "slack",
  "matrix",
  "irc",
  "whatsapp",
] as const;

type SupportedChannel = (typeof SUPPORTED_CHANNELS)[number];

function loadPrompts(): Record<string, string> {
  const out: Record<string, string> = {};
  for (const ch of SUPPORTED_CHANNELS) {
    const file = path.join(PROMPTS_DIR, `${ch}.md`);
    try {
      const content = fs.readFileSync(file, "utf-8").trim();
      if (content) out[ch] = content;
      else console.error(`[formatting] empty prompt file for '${ch}': ${file}`);
    } catch (e) {
      // Surfacing the failure loudly: a missing file silently disables the
      // formatting prompt for that channel (LLM gets no guidance).
      console.error(`[formatting] failed to load prompt for '${ch}' from ${file}:`, e);
    }
  }
  return out;
}

const PROMPTS: Record<string, string> = loadPrompts();

/** Get channel-specific formatting prompt for the LLM system prompt. */
export function getFormattingPrompt(channelType: string): string | undefined {
  return PROMPTS[channelType];
}
