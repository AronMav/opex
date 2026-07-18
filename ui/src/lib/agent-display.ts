/**
 * Shared helper for rendering a safe, human-facing agent participant label.
 *
 * WS6 defense-in-depth: when a session gets silently recreated, some code
 * paths can end up threading a raw session UUID (or any id that isn't a
 * currently-configured agent) through as `agentId`. Never surface that raw
 * value to the user — fall back to a generic localized label instead.
 */

import type { TranslationKey } from "@/i18n/types";

const UUID_SHAPE_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function displayAgentName(
  agentId: string,
  knownAgents: readonly string[],
  t: (key: TranslationKey) => string,
): string {
  if (UUID_SHAPE_RE.test(agentId) || !knownAgents.includes(agentId)) {
    return t("chat.unknown_agent");
  }
  return agentId;
}
