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
