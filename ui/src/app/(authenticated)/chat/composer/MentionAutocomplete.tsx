"use client";

import { useState, useEffect } from "react";
import { useTranslation } from "@/hooks/use-translation";

export const MENTION_OPTION_ID_PREFIX = "mention-option-";

export function MentionAutocomplete({ query, agents, onSelect, onClose, onActiveChange, listboxId }: {
  query: string;
  agents: string[];
  onSelect: (name: string) => void;
  /** Close the menu (Escape). Optional — omitted in isolated unit tests. */
  onClose?: () => void;
  /** Reports the active option's DOM id (or null when closed) so the composer
   *  textarea can mirror it via aria-activedescendant. */
  onActiveChange?: (optionId: string | null) => void;
  /** id for the listbox element so the composer textarea can point at it via
   *  aria-controls (WAI-ARIA combobox pattern). */
  listboxId?: string;
}) {
  const { t } = useTranslation();
  const q = query.toLowerCase();
  const filtered = agents.filter(p => p.toLowerCase().startsWith(q));
  const [activeIdx, setActiveIdx] = useState(0);

  useEffect(() => { setActiveIdx(0); }, [query]);

  // Keep the composer's aria-activedescendant in sync; clear it on unmount.
  useEffect(() => {
    onActiveChange?.(filtered.length > 0 ? `${MENTION_OPTION_ID_PREFIX}${Math.min(activeIdx, filtered.length - 1)}` : null);
  }, [activeIdx, filtered.length, onActiveChange]);
  useEffect(() => () => onActiveChange?.(null), [onActiveChange]);

  // Capture-phase keydown so ArrowDown/ArrowUp/Enter/Tab/Escape drive the menu
  // instead of the textarea (which would otherwise submit the half-typed "@").
  // Mirrors CommandAutocomplete's keydown handler.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (filtered.length === 0) return;
      // Only act while the keystroke originates inside the composer — a stray
      // "@" left in the box must not let this menu hijack keys typed in an
      // unrelated control once focus has moved away. (Unit tests dispatch on
      // `window`, whose target isn't an HTMLElement, so the guard is skipped.)
      if (e.target instanceof HTMLElement && !e.target.closest("[data-composer-input]")) return;
      // stopPropagation (capture, on window) prevents the event from ever
      // reaching the textarea's React onKeyDown — so selecting a mention with
      // Enter can never also submit the half-typed "@" (the C2 bug). Unlike the
      // slash menu, selecting a mention leaves text in the box, so the textarea
      // submit MUST be blocked here rather than relying on an empty-text no-op.
      if (e.key === "ArrowDown") { e.preventDefault(); e.stopPropagation(); setActiveIdx(i => (i + 1) % filtered.length); }
      if (e.key === "ArrowUp")   { e.preventDefault(); e.stopPropagation(); setActiveIdx(i => (i - 1 + filtered.length) % filtered.length); }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        e.stopPropagation();
        const safeIdx = Math.min(activeIdx, filtered.length - 1);
        if (filtered[safeIdx]) onSelect(filtered[safeIdx]);
      }
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose?.(); }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [filtered, activeIdx, onSelect, onClose]);

  if (filtered.length === 0) return null;

  return (
    <div
      role="listbox"
      id={listboxId}
      aria-label={t("chat.mentions_label")}
      className="absolute bottom-full mb-1 left-0 max-h-[50dvh] overflow-y-auto bg-popover border border-border rounded-lg shadow-lg p-1 z-50 w-full max-w-[min(280px,calc(100dvw-1.5rem))]"
    >
      {filtered.map((name, i) => (
        <button
          key={name}
          role="option"
          id={`${MENTION_OPTION_ID_PREFIX}${i}`}
          aria-selected={i === activeIdx}
          className={`flex items-center gap-2 px-3 py-1.5 text-sm rounded-md hover:bg-muted w-full text-left ${i === activeIdx ? "bg-muted/50" : ""}`}
          onMouseDown={(e) => { e.preventDefault(); onSelect(name); }}
        >
          <span className="font-semibold">@{name}</span>
        </button>
      ))}
    </div>
  );
}
