/**
 * Pure parsing helper for the `cm:<token>:<value>` callback_data emitted by a
 * `command_args_menu` choice-valve keyboard (Core `argsmenu_buttons` in
 * `crates/opex-core/src/agent/engine/run.rs`). Mirrors the existing
 * `hm:<token>:<handler_id>` handler-menu parsing inline in telegram.ts.
 *
 * `token` is the 32-hex-char capability token minted by `store_menu_ctx`
 * (`Uuid::simple()`); `value` is everything after the 2nd colon, rejoined in
 * case a choice value itself contains a colon.
 */
export function parseCmCallback(data: string): { token: string; value: string } | null {
  if (!data.startsWith("cm:")) return null;
  const parts = data.split(":");
  if (parts.length < 3) return null;
  const token = parts[1];
  const value = parts.slice(2).join(":");
  if (!token || !value) return null;
  return { token, value };
}

/**
 * Pure parsing helper for the `hm:<token>:<handler_id>` handler-menu callback_data
 * (Core `handler_menu` → `send_buttons`). Same shape/guards as `parseCmCallback`;
 * rejoins a colon-containing handler_id and rejects empty token/id — replacing the
 * hand-rolled inline `data.split(":")` that had no such guard.
 */
export function parseHmCallback(data: string): { token: string; handlerId: string } | null {
  if (!data.startsWith("hm:")) return null;
  const parts = data.split(":");
  if (parts.length < 3) return null;
  const token = parts[1];
  const handlerId = parts.slice(2).join(":");
  if (!token || !handlerId) return null;
  return { token, handlerId };
}
