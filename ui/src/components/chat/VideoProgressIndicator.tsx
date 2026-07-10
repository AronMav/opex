"use client";
import { useChatStore } from "@/stores/chat-store";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { Loader2 } from "lucide-react";

// Machine phase keys emitted by file handlers via ctx.progress(phase, pct)
// (summarize_video / transcribe: fetch → transcribe → fix_terms → digest → saving). The
// backend sends the raw key over the `file_job_progress` WS event; we localise
// it here so the indicator never shows an untranslated "fetch" to the user.
// Unknown phases fall through to the raw text the store already holds.
const PHASES: Record<string, { emoji: string; key: TranslationKey }> = {
  fetch: { emoji: "📥", key: "chat.video_phase_download" },
  transcribe: { emoji: "🎙️", key: "chat.video_phase_transcribe" },
  fix_terms: { emoji: "🔎", key: "chat.video_phase_fix_terms" },
  digest: { emoji: "🧠", key: "chat.video_phase_digest" },
  saving: { emoji: "💾", key: "chat.video_phase_saving" },
};

export function VideoProgressIndicator({ sessionId }: { sessionId: string | null }) {
  const entry = useChatStore((s) => (sessionId ? s.videoProgress[sessionId] : undefined));
  const { t } = useTranslation();
  if (!entry) return null;
  const phase = PHASES[entry.phase];
  return (
    <div role="status" aria-live="polite" className="flex items-center gap-2 px-4 py-2 text-sm text-muted-foreground">
      <Loader2 className="h-4 w-4 animate-spin" aria-hidden />
      {phase ? (
        <span>
          <span aria-hidden="true">{phase.emoji} </span>
          {t(phase.key)}
        </span>
      ) : (
        <span>{entry.text}</span>
      )}
    </div>
  );
}
