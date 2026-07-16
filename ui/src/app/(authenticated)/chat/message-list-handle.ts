// ── message-list-handle.ts ───────────────────────────────────────────────────
// Module-level registry for the chat MessageList's Virtuoso handle.
//
// The jump-to-message flow (use-scroll-to-message.ts) needs to imperatively
// scroll the virtualised message list to a given index. Prop-drilling a ref
// through ChatThread → MessageList (and back up to the hook that lives on
// ChatThread) is awkward, so MessageList registers its VirtuosoHandle here on
// mount and the hook calls scrollToMessageIndex() directly. Single consumer,
// single producer — a plain module variable is sufficient (no reactivity
// needed; the caller already knows WHEN to scroll).

import type { VirtuosoHandle } from "react-virtuoso";

let handle: VirtuosoHandle | null = null;

/** Registered by MessageList on mount; cleared on unmount. */
export function setVirtuosoHandle(h: VirtuosoHandle | null): void {
  handle = h;
}

/**
 * Scroll the message list so the item at `index` (position in the SAME array
 * Virtuoso renders) is centred. No-op if the list is not currently mounted.
 */
export function scrollToMessageIndex(index: number): void {
  if (!handle || index < 0) return;
  handle.scrollToIndex({ index, align: "center", behavior: "smooth" });
}
