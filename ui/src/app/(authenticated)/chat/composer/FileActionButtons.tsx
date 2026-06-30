"use client";

import React, { useEffect, useState, useCallback } from "react";
import { apiGet, apiPost } from "@/lib/api";
import { useLanguageStore } from "@/stores/language-store";
import type { FileActionButton, FileActionsResponse } from "@/types/api";
import { Loader2, Mic, Image as ImageIcon, FileText, Save, Video, Wand2 } from "lucide-react";

interface FileActionButtonsProps {
  // upload ROW UUID (the `filename` returned by POST /api/media/upload), NOT a URL path.
  uploadId: string;
  mime: string;
  agent: string;
  sessionId: string | null;
}

// ── Icon lookup keyed by the descriptor's <icon> string. Unknown → generic. ──

const ICONS: Record<string, React.ComponentType<{ className?: string }>> = {
  mic: Mic,
  image: ImageIcon,
  document: FileText,
  save: Save,
  video: Video,
};

function IconFor({ name }: { name: string }) {
  const Cmp = ICONS[name] ?? Wand2;
  return <Cmp className="h-3.5 w-3.5" />;
}

export function FileActionButtons({ uploadId, mime, agent, sessionId }: FileActionButtonsProps) {
  // locale drives the server-side label localization (re-fetch when it changes).
  const locale = useLanguageStore((s) => s.locale);
  const [buttons, setButtons] = useState<FileActionButton[]>([]);
  const [running, setRunning] = useState<string | null>(null);

  useEffect(() => {
    if (!uploadId) {
      setButtons([]);
      return;
    }
    let cancelled = false;
    const session = sessionId ?? "";
    const qs = `agent=${encodeURIComponent(agent)}&session=${encodeURIComponent(session)}`;
    apiGet<FileActionsResponse>(`/api/files/${encodeURIComponent(uploadId)}/actions?${qs}`)
      .then((resp) => {
        if (!cancelled) setButtons(resp.buttons ?? []);
      })
      .catch(() => {
        if (!cancelled) setButtons([]); // fail-soft: no buttons, file still attachable
      });
    return () => {
      cancelled = true;
    };
    // mime is included so the slot re-fetches if the same attachment id is reused
    // for a different file; locale re-fetches localized labels.
  }, [uploadId, agent, sessionId, mime, locale]);

  const run = useCallback(
    async (btn: FileActionButton) => {
      if (running) return;
      setRunning(btn.id);
      try {
        await apiPost(`/api/files/${encodeURIComponent(uploadId)}/run`, {
          handler_id: btn.id,
          params: btn.params,
          session_id: sessionId,
          agent,
        });
      } catch (err) {
        const { toast } = await import("sonner");
        toast.error(err instanceof Error ? err.message : "run failed");
      } finally {
        setRunning(null);
      }
    },
    [running, uploadId, sessionId, agent],
  );

  if (buttons.length === 0) return null;

  return (
    <div className="flex flex-wrap items-center gap-1.5 px-3 pt-2">
      {buttons.map((btn) => (
        <button
          key={btn.id}
          type="button"
          disabled={running !== null}
          onClick={() => run(btn)}
          className="inline-flex items-center gap-1.5 rounded-lg border border-border/60 bg-muted/30 px-2.5 py-1 text-xs font-medium text-foreground/80 hover:bg-muted/60 hover:text-foreground transition-colors disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {running === btn.id ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <IconFor name={btn.icon} />}
          {btn.label}
        </button>
      ))}
    </div>
  );
}
