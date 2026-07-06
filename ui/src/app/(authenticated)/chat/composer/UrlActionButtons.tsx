"use client";

import React, { useEffect, useState, useCallback, useRef } from "react";
import { toast } from "sonner";
import { apiGet, apiPost } from "@/lib/api";
import { useLanguageStore } from "@/stores/language-store";
import { Button } from "@/components/ui/button";
import { queryClient } from "@/lib/query-client";
import { qk } from "@/lib/queries";
import type { FileActionButton, FileActionsResponse } from "@/types/api";
import { Loader2, Mic, Image as ImageIcon, FileText, Save, Video, Wand2, Link2 } from "lucide-react";

// ── YouTube URL detection (same regex as Telegram adapter) ─────────────────

const VIDEO_URL_RE = /https?:\/\/[^\s,)]+(?:youtube\.com\/watch\?v=|youtu\.be\/)([a-zA-Z0-9_-]+)/i;

function detectVideoUrl(text: string): string | null {
  const m = text.match(VIDEO_URL_RE);
  return m ? m[0] : null;
}

// ── Icon lookup (shared with FileActionButtons) ─────────────────────────────

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

// ── UrlActionButtons component ──────────────────────────────────────────────

interface UrlActionButtonsProps {
  /** The text to scan for video URLs (typically the composer textarea value). */
  text: string;
  agent: string;
  sessionId: string | null;
}

export function UrlActionButtons({ text, agent, sessionId }: UrlActionButtonsProps) {
  const locale = useLanguageStore((s) => s.locale);
  const [buttons, setButtons] = useState<FileActionButton[]>([]);
  const [running, setRunning] = useState<string | null>(null);
  const [detectedUrl, setDetectedUrl] = useState<string | null>(null);
  const runningRef = useRef(false);

  // Detect video URL in text and fetch matching handlers.
  useEffect(() => {
    const url = detectVideoUrl(text);
    setDetectedUrl(url);

    if (!url) {
      setButtons([]);
      return;
    }

    let cancelled = false;
    const lang = locale;
    apiGet<FileActionsResponse>(`/api/handlers/match-url?url=${encodeURIComponent(url)}&lang=${encodeURIComponent(lang)}`)
      .then((resp) => {
        if (!cancelled) setButtons(resp.buttons ?? []);
      })
      .catch(() => {
        if (!cancelled) setButtons([]);
      });
    return () => {
      cancelled = true;
    };
  }, [text, locale]);

  const run = useCallback(
    async (btn: FileActionButton) => {
      if (runningRef.current || !detectedUrl) return;
      runningRef.current = true;
      setRunning(btn.id);
      try {
        const result = await apiPost<Record<string, unknown>>("/api/handlers/enqueue", {
          source_url: detectedUrl,
          handler_id: btn.id,
          agent_name: agent,
          session_id: sessionId ?? "00000000-0000-0000-0000-000000000000",
          params: { language: locale },
        });
        const isAsync = result && result["accepted"] === true && "job_id" in result;
        if (!isAsync && sessionId) {
          queryClient.invalidateQueries({ queryKey: qk.sessionMessages(sessionId) });
        }
        toast.success("Задача запущена");
      } catch (err) {
        toast.error(err instanceof Error ? err.message : "run failed");
      } finally {
        runningRef.current = false;
        setRunning(null);
      }
    },
    [detectedUrl, sessionId, agent, locale],
  );

  if (buttons.length === 0 || !detectedUrl) return null;

  return (
    <div className="flex flex-wrap items-center gap-1.5 px-3 pt-2">
      <Link2 className="h-3.5 w-3.5 text-muted-foreground" />
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