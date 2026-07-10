"use client";
import { useEffect, useMemo, useRef, useState } from "react";
import type { CommandInfo } from "@/types/api";

export const COMMAND_OPTION_ID_PREFIX = "command-option-";

interface Props {
  input: string;
  commands: CommandInfo[];
  onPick: (name: string) => void;
  onClose: () => void;
  /** Reports the active option's DOM id (or null when closed) so the composer
   *  textarea can mirror it via aria-activedescendant (WAI-ARIA combobox). */
  onActiveChange?: (optionId: string | null) => void;
  /** id for the listbox element so the composer textarea can point at it via
   *  aria-controls. */
  listboxId?: string;
}

/** Registry-backed slash-command dropdown — the single slash menu in the composer,
 *  100% driven by the /api/commands registry (no hardcoded command list). Keyboard
 *  nav: ArrowUp/ArrowDown moves the active item (scrolling it into view), Enter/Tab
 *  picks it, Escape closes. The active item gets a strong, distinct highlight (not just the
 *  subtle hover tint) so keyboard selection is clearly visible. */
export function CommandAutocomplete({ input, commands, onPick, onClose, onActiveChange, listboxId }: Props) {
  const [activeIdx, setActiveIdx] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const isSlash = input.startsWith("/");
  const q = isSlash ? input.slice(1).toLowerCase() : "";
  // Memoized so ChatComposer's per-keystroke re-render doesn't rebuild the list
  // (and tear down/re-attach the keydown listener) on every keypress.
  const matches = useMemo(
    () =>
      isSlash
        ? commands.filter(
            (c) => c.name.toLowerCase().startsWith(q) || c.aliases.some((a) => a.toLowerCase().startsWith(q)),
          )
        : [],
    [isSlash, q, commands],
  );

  useEffect(() => { setActiveIdx(0); }, [input]);

  // Keep the composer's aria-activedescendant in sync; clear it on unmount.
  useEffect(() => {
    onActiveChange?.(
      matches.length > 0 ? `${COMMAND_OPTION_ID_PREFIX}${Math.min(activeIdx, matches.length - 1)}` : null,
    );
  }, [activeIdx, matches.length, onActiveChange]);
  useEffect(() => () => onActiveChange?.(null), [onActiveChange]);

  // Keep the keyboard-selected item scrolled into view — the list can overflow its
  // max-height, so ArrowDown past the fold must reveal the newly-active row.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`#${COMMAND_OPTION_ID_PREFIX}${activeIdx}`);
    el?.scrollIntoView?.({ block: "nearest" });
  }, [activeIdx]);

  useEffect(() => {
    if (matches.length === 0) return;
    const handler = (e: KeyboardEvent) => {
      // Only act while the keystroke originates inside the composer. A stray "/"
      // left in the textarea must NOT let this menu hijack Arrow/Enter/Escape in
      // an unrelated control elsewhere on the page once focus has moved away.
      // (In unit tests the event is dispatched on `window`, whose target isn't an
      // HTMLElement, so the guard is skipped and the menu stays testable.)
      if (e.target instanceof HTMLElement && !e.target.closest("[data-composer-input]")) return;
      if (e.key === "ArrowDown") { e.preventDefault(); setActiveIdx((i) => (i + 1) % matches.length); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setActiveIdx((i) => (i - 1 + matches.length) % matches.length); }
      else if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        const safeIdx = Math.min(activeIdx, matches.length - 1);
        if (matches[safeIdx]) onPick(matches[safeIdx].name);
      } else if (e.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [matches, activeIdx, onPick, onClose]);

  if (!isSlash || matches.length === 0) return null;

  return (
    <div
      ref={listRef}
      role="listbox"
      id={listboxId}
      aria-label="Slash commands"
      className="absolute bottom-full mb-1 w-full max-h-64 overflow-y-auto rounded-md border bg-popover shadow-md"
    >
      {matches.map((c, i) => {
        const active = i === activeIdx;
        return (
          <button
            key={c.name}
            type="button"
            role="option"
            aria-selected={active}
            id={`${COMMAND_OPTION_ID_PREFIX}${i}`}
            className={`flex w-full items-baseline gap-2 border-l-2 px-3 py-1.5 text-left transition-colors ${
              active
                ? "border-primary bg-accent font-medium text-accent-foreground"
                : "border-transparent hover:bg-accent/60"
            }`}
            onMouseEnter={() => setActiveIdx(i)}
            onMouseDown={(e) => { e.preventDefault(); onPick(c.name); }}
          >
            <span className="font-mono text-sm">/{c.name}</span>
            {c.args.length > 0 && (
              <span className={`font-mono text-xs ${active ? "text-accent-foreground/70" : "text-muted-foreground"}`}>
                {c.args.map((a) => `<${a.name}>`).join(" ")}
              </span>
            )}
            <span className={`ml-auto truncate text-xs ${active ? "text-accent-foreground/80" : "text-muted-foreground"}`}>
              {c.description}
            </span>
          </button>
        );
      })}
    </div>
  );
}
