"use client";

import React, { useEffect, useState, useCallback, useRef } from "react";
import { toast } from "sonner";
import { apiGet, apiPost } from "@/lib/api";
import { useLanguageStore } from "@/stores/language-store";
import { Button } from "@/components/ui/button";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";
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
  // Ref-based guard: prevents a second concurrent run without capturing `running`
  // in the useCallback dependency array (avoids stale-closure recreation on every state tick).
  const runningRef = useRef(false);

  useEffect(() => {
    if (!uploadId) {
      setButtons([]);
      return;
    }
    let cancelled = false;
    const session = sessionId ?? "";
    const qs = `agent=${encodeURIComponent(agent)}&session=${encodeURIComponent(session)}&lang=${encodeURIComponent(locale)}`;
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
      if (runningRef.current) return;
      runningRef.current = true;
      setRunning(btn.id);
      try {
        const result = await apiPost<Record<string, unknown>>(
          `/api/files/${encodeURIComponent(uploadId)}/run`,
          {
            handler_id: btn.id,
            params: {},
            session_id: sessionId,
            agent,
            lang: locale,
          },
        );
        // Async ack has `accepted: true` + `job_id`; sync result has a `status` field.
        // For sync runs (200), invalidate immediately so the persisted result appears.
        const isAsync = result && result["accepted"] === true && "job_id" in result;
        if (!isAsync && sessionId) {
          queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
        }
      } catch (err) {
        toast.error(err instanceof Error ? err.message : "run failed");
      } finally {
        runningRef.current = false;
        setRunning(null);
      }
    },
    [uploadId, sessionId, agent, locale],
  );

  if (buttons.length === 0) return null;

  return (
    <div className="flex flex-wrap items-center gap-1.5 px-3 pt-2">
      {buttons.map((btn) => (
        <Button
          key={btn.id}
          type="button"
          variant="outline"
          size="xs"
          disabled={running !== null}
          onClick={() => run(btn)}
        >
          {running === btn.id ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <IconFor name={btn.icon} />}
          {btn.label}
        </Button>
      ))}
    </div>
  );
}
