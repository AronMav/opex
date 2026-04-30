// ── hooks/use-message-search.ts ──────────────────────────────────────────────
// Per-session message search state. Pure hook — no store mutations.

import { useState, useMemo, useEffect } from "react";
import { searchMessages } from "@/stores/chat-history";
import type { SearchMatch } from "@/stores/chat-history";
import type { ChatMessage } from "@/stores/chat-types";

export interface UseMessageSearch {
  isOpen: boolean;
  query: string;
  matches: SearchMatch[];
  activeIndex: number;
  activeMatch: SearchMatch | null;
  setQuery: (q: string) => void;
  open: () => void;
  close: () => void;
  next: () => void;
  prev: () => void;
}

export function useMessageSearch(messages: ChatMessage[]): UseMessageSearch {
  const [isOpen, setIsOpen] = useState(false);
  const [query, setQueryRaw] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);

  const matches = useMemo(
    () => searchMessages(query, messages),
    [query, messages],
  );

  // Reset active index when query changes.
  const setQuery = (q: string) => {
    setQueryRaw(q);
    setActiveIndex(0);
  };

  // Clamp activeIndex when matches change.
  const clampedIndex =
    matches.length === 0 ? 0 : Math.min(activeIndex, matches.length - 1);
  const activeMatch = matches.length > 0 ? matches[clampedIndex] : null;

  // Scroll active match into view when it changes.
  useEffect(() => {
    if (!activeMatch) return;
    const el = document.getElementById(`msg-${activeMatch.messageId}`);
    if (el) el.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [activeMatch]);

  const open = () => setIsOpen(true);
  const close = () => {
    setIsOpen(false);
    setQueryRaw("");
    setActiveIndex(0);
  };

  const next = () => {
    if (matches.length === 0) return;
    setActiveIndex((i) => (i + 1) % matches.length);
  };

  const prev = () => {
    if (matches.length === 0) return;
    setActiveIndex((i) => (i - 1 + matches.length) % matches.length);
  };

  return {
    isOpen,
    query,
    matches,
    activeIndex: clampedIndex,
    activeMatch,
    setQuery,
    open,
    close,
    next,
    prev,
  };
}
