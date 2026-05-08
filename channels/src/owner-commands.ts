// channels/src/owner-commands.ts
//
// Generic owner-command parsing/dispatch for non-Telegram, non-Discord
// drivers (Slack, Matrix, IRC, Email, WhatsApp). Telegram and Discord both
// have rich-message replies that benefit from native APIs and inline
// keyboards, so they keep their own copies of the dispatch loop. The other
// drivers all reduce to "post a string back to the user", so they can share
// this helper.
//
// Audit 2026-05-08 (5th pass): Slack and Matrix called bridge.checkAccess()
// and computed isOwner, but never gated on it — owners on those channels
// could not approve pairing requests, list users, or revoke access. IRC had
// no owner-command branch at all, leaving the bot effectively un-bootstrapable
// over IRC. This helper plugs that gap with a single shared implementation.

import type { BridgeHandle } from "./bridge";
import type { Strings } from "./localization";

/** Returns `true` if `text` matched and was handled (caller should `return`). */
export function isOwnerCommand(text: string): boolean {
  const t = text.trim();
  return (
    t.startsWith("/approve ") ||
    t.startsWith("/reject ") ||
    t === "/users" ||
    t.startsWith("/revoke ")
  );
}

/** Build the response text for an owner command. Returns `null` if the text
 *  was not an owner command (caller should fall through to normal flow). */
export async function runOwnerCommand(
  text: string,
  bridge: BridgeHandle,
  strings: Strings,
): Promise<string | null> {
  const trimmed = text.trim();

  if (trimmed.startsWith("/approve ")) {
    const code = trimmed.slice("/approve ".length).trim();
    const result = await bridge.approvePairing(code);
    return result.success ? strings.userApproved(code) : strings.codeNotFound;
  }
  if (trimmed.startsWith("/reject ")) {
    const code = trimmed.slice("/reject ".length).trim();
    bridge.rejectPairing(code);
    return strings.requestRejected;
  }
  if (trimmed === "/users") {
    const users = await bridge.listUsers();
    if (users.length === 0) {
      return strings.noApprovedUsers;
    }
    let out = strings.approvedUsersHeader;
    for (const u of users) {
      const uid = u.channel_user_id ?? "?";
      const label = u.display_name ?? uid;
      out += strings.userListItem(label, uid, u.approved_at ?? "?");
    }
    out += strings.revokeHint;
    return out;
  }
  if (trimmed.startsWith("/revoke ")) {
    const targetId = trimmed.slice("/revoke ".length).trim();
    const success = await bridge.revokeUser(targetId);
    return success ? strings.userRevoked(targetId) : strings.userNotFound;
  }
  return null;
}
