"use client";
import { useEffect, useState } from "react";
import { useTranslation } from "@/hooks/use-translation";

const SLASH_COMMAND_KEYS = [
  { cmd: "/new",     key: "chat.slash_new" },
  { cmd: "/reset",   key: "chat.slash_reset" },
  { cmd: "/compact", key: "chat.slash_compact" },
  { cmd: "/stop",    key: "chat.slash_stop" },
  { cmd: "/think:0", key: "chat.slash_think_off" },
  { cmd: "/think:1", key: "chat.slash_think_min" },
  { cmd: "/think:3", key: "chat.slash_think_med" },
  { cmd: "/think:5", key: "chat.slash_think_max" },
] as const;

interface Props {
  query: string;
  onSelect: (cmd: string) => void;
  onClose: () => void;
}

export function SlashMenu({ query, onSelect, onClose }: Props) {
  const { t } = useTranslation();
  const SLASH_COMMANDS = SLASH_COMMAND_KEYS.map(({ cmd, key }) => ({ cmd, description: t(key) }));
  const [activeIdx, setActiveIdx] = useState(0);
  const filtered = SLASH_COMMANDS.filter(c => c.cmd.startsWith(query));

  useEffect(() => { setActiveIdx(0); }, [query]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (filtered.length === 0) return;
      if (e.key === "ArrowDown") { e.preventDefault(); setActiveIdx(i => (i + 1) % filtered.length); }
      if (e.key === "ArrowUp")   { e.preventDefault(); setActiveIdx(i => (i - 1 + filtered.length) % filtered.length); }
      if (e.key === "Enter") {
        e.preventDefault();
        const safeIdx = Math.min(activeIdx, filtered.length - 1);
        if (filtered[safeIdx]) onSelect(filtered[safeIdx].cmd);
      }
      if (e.key === "Escape")    { onClose(); }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [filtered, activeIdx, onSelect, onClose]);

  if (filtered.length === 0) return null;

  return (
    <div
      role="listbox"
      aria-label={t("chat.slash_stop")}
      className="absolute bottom-full mb-2 left-0 z-50 w-72 max-w-[calc(100dvw-1.5rem)] max-h-[40dvh] overflow-y-auto rounded-xl border border-border bg-card shadow-lg"
    >
      {filtered.map((item, i) => (
        <button
          key={item.cmd}
          role="option"
          aria-selected={i === activeIdx}
          id={`slash-option-${i}`}
          className={`w-full flex items-center gap-3 px-3 py-2 text-sm text-left hover:bg-muted/50 transition-colors ${i === activeIdx ? "bg-muted/50" : ""}`}
          onMouseDown={(e) => { e.preventDefault(); onSelect(item.cmd); }}
        >
          <span className="font-mono text-primary font-medium">{item.cmd}</span>
          <span className="text-muted-foreground text-xs">{item.description}</span>
        </button>
      ))}
    </div>
  );
}
