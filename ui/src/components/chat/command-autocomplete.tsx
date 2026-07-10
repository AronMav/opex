"use client";
import { useEffect, useState } from "react";
import type { CommandInfo } from "@/types/api";

interface Props {
  input: string;
  commands: CommandInfo[];
  onPick: (name: string) => void;
  onClose: () => void;
}

/** Registry-backed slash-command dropdown — the single slash menu in the composer,
 *  100% driven by the /api/commands registry (no hardcoded command list). Keyboard
 *  nav: ArrowUp/ArrowDown moves the active item, Enter picks it, Escape closes. */
export function CommandAutocomplete({ input, commands, onPick, onClose }: Props) {
  const [activeIdx, setActiveIdx] = useState(0);
  const isSlash = input.startsWith("/");
  const q = isSlash ? input.slice(1).toLowerCase() : "";
  const matches = isSlash
    ? commands.filter(
        (c) => c.name.toLowerCase().startsWith(q) || c.aliases.some((a) => a.toLowerCase().startsWith(q)),
      )
    : [];

  useEffect(() => { setActiveIdx(0); }, [input]);

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
      role="listbox"
      aria-label="Slash commands"
      className="absolute bottom-full mb-1 w-full max-h-64 overflow-y-auto rounded-md border bg-popover shadow-md"
    >
      {matches.map((c, i) => (
        <button
          key={c.name}
          type="button"
          role="option"
          aria-selected={i === activeIdx}
          id={`command-option-${i}`}
          className={`flex w-full items-baseline gap-2 px-3 py-1.5 text-left hover:bg-accent ${i === activeIdx ? "bg-accent" : ""}`}
          onMouseDown={(e) => { e.preventDefault(); onPick(c.name); }}
        >
          <span className="font-mono text-sm">/{c.name}</span>
          {c.args.length > 0 && (
            <span className="font-mono text-xs text-muted-foreground">
              {c.args.map((a) => `<${a.name}>`).join(" ")}
            </span>
          )}
          <span className="ml-auto truncate text-xs text-muted-foreground">{c.description}</span>
        </button>
      ))}
    </div>
  );
}
