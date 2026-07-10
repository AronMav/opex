"use client";
import { useEffect, useRef, useState } from "react";
import type { CommandInfo } from "@/types/api";

interface Props {
  input: string;
  commands: CommandInfo[];
  onPick: (name: string) => void;
  onClose: () => void;
}

/** Registry-backed slash-command dropdown — the single slash menu in the composer,
 *  100% driven by the /api/commands registry (no hardcoded command list). Keyboard
 *  nav: ArrowUp/ArrowDown moves the active item (scrolling it into view), Enter picks
 *  it, Escape closes. The active item gets a strong, distinct highlight (not just the
 *  subtle hover tint) so keyboard selection is clearly visible. */
export function CommandAutocomplete({ input, commands, onPick, onClose }: Props) {
  const [activeIdx, setActiveIdx] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const isSlash = input.startsWith("/");
  const q = isSlash ? input.slice(1).toLowerCase() : "";
  const matches = isSlash
    ? commands.filter(
        (c) => c.name.toLowerCase().startsWith(q) || c.aliases.some((a) => a.toLowerCase().startsWith(q)),
      )
    : [];

  useEffect(() => { setActiveIdx(0); }, [input]);

  // Keep the keyboard-selected item scrolled into view — the list can overflow its
  // max-height, so ArrowDown past the fold must reveal the newly-active row.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`#command-option-${activeIdx}`);
    el?.scrollIntoView?.({ block: "nearest" });
  }, [activeIdx]);

  useEffect(() => {
    if (matches.length === 0) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") { e.preventDefault(); setActiveIdx((i) => (i + 1) % matches.length); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setActiveIdx((i) => (i - 1 + matches.length) % matches.length); }
      else if (e.key === "Enter") {
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
            id={`command-option-${i}`}
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
