// ui/src/components/chat/CompactChainBanner.tsx
"use client";
import { useState, useEffect } from "react";
import { ChevronDown, ChevronUp, Shrink } from "lucide-react";
import { useSessionChain } from "@/lib/queries";
import type { SessionChainEntry } from "@/types/api";

interface CompactChainBannerProps {
  /** The currently active session ID. */
  activeSessionId: string;
  /** Called when the user clicks a chain entry to navigate there. */
  onNavigate: (sessionId: string) => void;
}

const STORAGE_KEY = "hydeclaw:chain-banner-collapsed";

export function CompactChainBanner({ activeSessionId, onNavigate }: CompactChainBannerProps) {
  const { data } = useSessionChain(activeSessionId);
  const [collapsed, setCollapsed] = useState(() => {
    try { return localStorage.getItem(STORAGE_KEY) === "1"; } catch { return false; }
  });

  useEffect(() => {
    try { localStorage.setItem(STORAGE_KEY, collapsed ? "1" : "0"); } catch {}
  }, [collapsed]);

  const chain = data?.chain ?? [];

  // Only show when session has a parent (i.e. is part of a chain)
  // Root sessions (no parent_session_id) do not show the banner.
  const currentEntry = chain.find((e) => e.id === activeSessionId);
  if (!currentEntry?.parent_session_id) return null;
  if (chain.length < 2) return null;

  return (
    <div className="border-b border-border bg-muted/30 text-xs">
      <button
        className="w-full flex items-center gap-2 px-3 py-1.5 hover:bg-muted/50 transition-colors text-left"
        onClick={() => setCollapsed((c) => !c)}
      >
        <Shrink className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
        <span className="font-medium text-foreground">Compression chain</span>
        <span className="text-muted-foreground ml-1">({chain.length} sessions)</span>
        <span className="ml-auto text-muted-foreground">
          {collapsed ? <ChevronDown className="h-3.5 w-3.5" /> : <ChevronUp className="h-3.5 w-3.5" />}
        </span>
      </button>

      {!collapsed && (
        <div className="px-3 pb-2 space-y-0.5">
          {chain.map((entry: SessionChainEntry) => {
            const isCurrent = entry.id === activeSessionId;
            return (
              <button
                key={entry.id}
                onClick={() => !isCurrent && onNavigate(entry.id)}
                disabled={isCurrent}
                className={[
                  "w-full flex items-center gap-2 py-1 px-1 rounded text-left transition-colors",
                  isCurrent
                    ? "font-semibold text-foreground cursor-default"
                    : "text-muted-foreground hover:text-foreground hover:bg-muted/50 cursor-pointer",
                ].join(" ")}
              >
                <span className="truncate flex-1">
                  {entry.title ?? `session ${entry.id.slice(0, 8)}`}
                </span>
                <span className="text-[10px] text-muted-foreground shrink-0">
                  {new Date(entry.started_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                </span>
                {entry.end_reason === "compression" && (
                  <span className="text-[10px] text-orange-500 shrink-0">↩</span>
                )}
                {isCurrent && (
                  <span className="text-[10px] text-primary shrink-0">←</span>
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
