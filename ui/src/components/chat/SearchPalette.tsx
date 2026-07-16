"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useRouter, usePathname } from "next/navigation";
import { Loader2, Search as SearchIcon } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";
import { useChatStore } from "@/stores/chat-store";
import { usePaletteStore } from "@/stores/palette-store";
import { searchAll } from "@/lib/search-api";
import type { SearchMessageHit, SearchSessionHit } from "@/types/api";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import { useHotkey } from "@/hooks/use-hotkey";

const ALL_AGENTS_KEY = "palette_all_agents";
const DEBOUNCE_MS = 250;
const MIN_QUERY_LEN = 2;

type PaletteRow =
  | { kind: "session"; item: SearchSessionHit }
  | { kind: "message"; item: SearchMessageHit };

function readAllAgentsPref(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(ALL_AGENTS_KEY) === "1";
  } catch {
    return false;
  }
}

/** Splits an FTS snippet on literal `<b>`/`</b>` markers and renders the
 *  matched spans as `<mark>`. The markers are treated as plain-text tokens
 *  (split + string compare) — never parsed as HTML, so this is safe against
 *  a snippet containing attacker-controlled markup. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/(<b>|<\/b>)/);
  let marking = false;
  const nodes: ReactNode[] = [];
  parts.forEach((part, i) => {
    if (part === "<b>") { marking = true; return; }
    if (part === "</b>") { marking = false; return; }
    if (!part) return;
    nodes.push(
      marking
        ? <mark key={i} className="bg-primary/20 text-foreground rounded-sm">{part}</mark>
        : <span key={i}>{part}</span>,
    );
  });
  return <>{nodes}</>;
}

/**
 * Ctrl+K search palette — cross-session/cross-agent full-text search over
 * message history and session titles. Self-contained: subscribes to
 * `usePaletteStore` for its own open state (toggled by the global Ctrl+K
 * listener in app/layout.tsx).
 *
 * Selection currently only closes the palette — `handleSelect` is the single
 * extension point Task 4 wires up for jump-to-message navigation (it will
 * call `usePaletteStore.getState().setTarget(...)` there).
 */
export function SearchPalette() {
  const { t } = useTranslation();
  const open = usePaletteStore((s) => s.open);
  const setOpen = usePaletteStore((s) => s.setOpen);
  const currentAgent = useChatStore((s) => s.currentAgent);
  const router = useRouter();
  const pathname = usePathname();

  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [allAgents, setAllAgents] = useState(false);
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<{ sessions: SearchSessionHit[]; messages: SearchMessageHit[] } | null>(null);
  const [activeIdx, setActiveIdx] = useState(0);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const requestSeq = useRef(0);

  // Hydrate the "search all agents" toggle from localStorage on mount.
  useEffect(() => {
    setAllAgents(readAllAgentsPref());
  }, []);

  // Global Ctrl+K / Cmd+K hotkey — registered here (rather than in the root
  // layout, which must stay a Server Component for its `metadata`/`viewport`
  // exports) so the palette is a fully self-contained drop-in next to the
  // toaster: mount it once, get the hotkey for free. The palette OWNS Ctrl+K
  // (the old focus-composer binding in chat/page.tsx was removed — "/" covers
  // that now). `allowInInput: true` is intentional: a command palette must be
  // able to open from anywhere, including while a textarea/input has focus
  // (Slack/Linear standard), not just when focus is elsewhere on the page.
  useHotkey(
    "k",
    (e) => {
      e.preventDefault();
      setOpen(true);
    },
    { ctrlOrMeta: true, allowInInput: true },
  );

  // Reset transient state each time the palette opens; cancel any pending
  // debounce timer when it closes.
  useEffect(() => {
    if (open) {
      setQuery("");
      setDebounced("");
      setResult(null);
      setActiveIdx(0);
    } else if (timerRef.current) {
      clearTimeout(timerRef.current);
    }
  }, [open]);

  // Debounce: only fire the search after DEBOUNCE_MS of no typing, and only
  // once the query reaches MIN_QUERY_LEN.
  useEffect(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    const trimmed = query.trim();
    if (trimmed.length < MIN_QUERY_LEN) {
      setDebounced("");
      setResult(null);
      setLoading(false);
      return;
    }
    timerRef.current = setTimeout(() => setDebounced(trimmed), DEBOUNCE_MS);
    return () => { if (timerRef.current) clearTimeout(timerRef.current); };
  }, [query]);

  // Fire the search whenever the debounced query, agent scope, or the
  // all-agents toggle changes. A request sequence guards against a slow
  // earlier response clobbering a faster later one.
  useEffect(() => {
    if (!debounced) return;
    const seq = ++requestSeq.current;
    setLoading(true);
    searchAll(debounced, allAgents ? { all: true } : { agent: currentAgent })
      .then((res) => {
        if (requestSeq.current !== seq) return;
        setResult({ sessions: res.sessions, messages: res.messages });
        setActiveIdx(0);
      })
      .catch(() => {
        if (requestSeq.current !== seq) return;
        setResult({ sessions: [], messages: [] });
      })
      .finally(() => {
        if (requestSeq.current === seq) setLoading(false);
      });
  }, [debounced, allAgents, currentAgent]);

  const toggleAllAgents = useCallback((v: boolean) => {
    setAllAgents(v);
    try {
      window.localStorage.setItem(ALL_AGENTS_KEY, v ? "1" : "0");
    } catch {
      // localStorage unavailable (private mode, quota) — toggle still works
      // for the rest of this session, it just won't persist across reloads.
    }
  }, []);

  const rows: PaletteRow[] = useMemo(
    () =>
      result
        ? [
            ...result.sessions.map((item): PaletteRow => ({ kind: "session", item })),
            ...result.messages.map((item): PaletteRow => ({ kind: "message", item })),
          ]
        : [],
    [result],
  );

  const handleSelect = useCallback((row: PaletteRow) => {
    const agentId = row.item.agent_id;
    const sessionId = row.item.session_id;

    // Message rows carry a specific message to jump to — set the target
    // BEFORE navigating so use-scroll-to-message (Task 3) picks it up as
    // soon as this session's history lands, regardless of which of the two
    // navigation paths below is taken. Session rows just land on the
    // session with no particular message highlighted.
    if (row.kind === "message") {
      usePaletteStore.getState().setTarget({ sessionId, messageId: row.item.message_id });
    }

    if (agentId === currentAgent && pathname === "/chat") {
      // Same agent, already on the chat page — switch sessions in place via
      // the store action (no route change, no remount).
      useChatStore.getState().selectSession(sessionId, agentId);
    } else {
      // Different agent, or the palette was opened from a non-chat page —
      // route there directly. use-session-restore's `?agent=&s=` deep-link
      // resolver (same mechanism as shared URLs) takes it from here.
      router.push(`/chat?agent=${encodeURIComponent(agentId)}&s=${sessionId}`);
    }

    setOpen(false);
  }, [setOpen, currentAgent, pathname, router]);

  useEffect(() => {
    if (!open || rows.length === 0) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveIdx((i) => (i + 1) % rows.length);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveIdx((i) => (i - 1 + rows.length) % rows.length);
      } else if (e.key === "Enter") {
        e.preventDefault();
        const safeIdx = Math.min(activeIdx, rows.length - 1);
        handleSelect(rows[safeIdx]);
      }
      // Escape is intentionally NOT handled here — Radix Dialog already
      // closes on Escape, so a second handler would be redundant.
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, rows, activeIdx, handleSelect, setOpen]);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent size="lg" layout="panel" className="max-h-[70dvh]" showCloseButton={false}>
        <DialogTitle className="sr-only">{t("palette.placeholder")}</DialogTitle>
        <div className="flex items-center gap-3 border-b border-border p-4 pb-3">
          <SearchIcon className="size-4 shrink-0 text-muted-foreground-subtle" aria-hidden="true" />
          <Input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("palette.placeholder")}
            className="h-auto border-0 px-0 shadow-none focus-visible:ring-0"
          />
          {loading && (
            <Loader2
              className="size-4 shrink-0 animate-spin text-muted-foreground-subtle"
              aria-label={t("palette.loading")}
            />
          )}
        </div>
        <div className="flex items-center justify-between border-b border-border px-4 py-2">
          <label htmlFor="palette-all-agents" className="text-sm text-muted-foreground">
            {t("palette.all_agents")}
          </label>
          <Switch id="palette-all-agents" checked={allAgents} onCheckedChange={toggleAllAgents} size="sm" />
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          {(!result || rows.length === 0) && !loading && (
            <p className="py-8 text-center text-sm text-muted-foreground-subtle">{t("palette.empty")}</p>
          )}
          {result && result.sessions.length > 0 && (
            <div className="mb-2">
              <h4 className="px-2 py-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground-subtle">
                {t("palette.sessions")}
              </h4>
              {result.sessions.map((s, i) => {
                const active = i === activeIdx;
                return (
                  <button
                    key={s.session_id}
                    type="button"
                    className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
                      active ? "bg-accent text-accent-foreground" : "hover:bg-accent/60"
                    }`}
                    onMouseEnter={() => setActiveIdx(i)}
                    onMouseDown={(e) => { e.preventDefault(); handleSelect({ kind: "session", item: s }); }}
                  >
                    <span className="flex-1 truncate">{s.title ?? s.session_id}</span>
                    {allAgents && <Badge variant="outline" size="xs">{s.agent_id}</Badge>}
                  </button>
                );
              })}
            </div>
          )}
          {result && result.messages.length > 0 && (
            <div>
              <h4 className="px-2 py-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground-subtle">
                {t("palette.messages")}
              </h4>
              {result.messages.map((m, i) => {
                const idx = result.sessions.length + i;
                const active = idx === activeIdx;
                return (
                  <button
                    key={m.message_id}
                    type="button"
                    className={`flex w-full flex-col gap-0.5 rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
                      active ? "bg-accent text-accent-foreground" : "hover:bg-accent/60"
                    }`}
                    onMouseEnter={() => setActiveIdx(idx)}
                    onMouseDown={(e) => { e.preventDefault(); handleSelect({ kind: "message", item: m }); }}
                  >
                    <div className="flex items-center gap-2">
                      <span className="flex-1 truncate text-muted-foreground">{m.session_title ?? m.session_id}</span>
                      {allAgents && <Badge variant="outline" size="xs">{m.agent_id}</Badge>}
                    </div>
                    <div className="truncate">
                      <Snippet text={m.snippet} />
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
