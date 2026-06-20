"use client";

// ── SearchBar.tsx ─────────────────────────────────────────────────────────────
// Inline per-session message search bar.

import React, { useRef, useEffect, useCallback } from "react";
import { X, ChevronUp, ChevronDown } from "lucide-react";
import type { UseMessageSearch } from "./hooks/use-message-search";
import { useTranslation } from "@/hooks/use-translation";

interface SearchBarProps {
  search: UseMessageSearch;
}

export function SearchBar({ search }: SearchBarProps) {
  const { t } = useTranslation();
  const { query, matches, activeIndex, setQuery, close, next, prev } = search;
  const inputRef = useRef<HTMLInputElement>(null);

  // Auto-focus when mounted.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (e.shiftKey) prev();
        else next();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        next();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        prev();
      }
    },
    [close, next, prev],
  );

  const counterText =
    matches.length === 0 && query
      ? t("chat.search_no_results")
      : matches.length > 0
        ? t("chat.search_counter", { current: activeIndex + 1, total: matches.length })
        : "";

  return (
    <div className="flex items-center gap-2 px-3 py-2 border-b border-border/50 bg-muted/20 animate-in slide-in-from-top-1 duration-150">
      <input
        ref={inputRef}
        type="text"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder={t("chat.search_messages")}
        className="flex-1 min-w-0 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground/50"
        aria-label={t("chat.search_messages")}
      />
      {counterText && (
        <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
          {counterText}
        </span>
      )}
      <button
        type="button"
        onClick={prev}
        disabled={matches.length === 0}
        aria-label={t("chat.search_prev")}
        className="shrink-0 rounded p-1 text-muted-foreground/60 hover:text-muted-foreground hover:bg-muted/50 transition-colors disabled:opacity-30"
      >
        <ChevronUp className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={next}
        disabled={matches.length === 0}
        aria-label={t("chat.search_next")}
        className="shrink-0 rounded p-1 text-muted-foreground/60 hover:text-muted-foreground hover:bg-muted/50 transition-colors disabled:opacity-30"
      >
        <ChevronDown className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={close}
        aria-label={t("chat.search_close")}
        className="shrink-0 rounded p-1 text-muted-foreground/60 hover:text-muted-foreground hover:bg-muted/50 transition-colors"
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}
