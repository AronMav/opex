"use client";
import { useChatStore } from "@/stores/chat-store";
import { useLanguageStore, type Locale } from "@/stores/language-store";
import { Loader2 } from "lucide-react";

// Machine phase keys emitted by file handlers via ctx.progress(phase, pct)
// (summarize_video / transcribe: fetch → transcribe → digest → saving). The
// backend sends the raw key over the `file_job_progress` WS event; we localise
// it here so the indicator never shows an untranslated "fetch" to the user.
// Unknown phases fall through to the raw text the store already holds.
const PHASE_LABELS: Record<string, Record<Locale, string>> = {
  fetch: { ru: "📥 Загружаю…", en: "📥 Downloading…" },
  transcribe: { ru: "🎙️ Транскрибирую…", en: "🎙️ Transcribing…" },
  digest: { ru: "🧠 Составляю конспект…", en: "🧠 Summarizing…" },
  saving: { ru: "💾 Сохраняю…", en: "💾 Saving…" },
};

export function VideoProgressIndicator({ sessionId }: { sessionId: string | null }) {
  const entry = useChatStore((s) => (sessionId ? s.videoProgress[sessionId] : undefined));
  const locale = useLanguageStore((s) => s.locale);
  if (!entry) return null;
  const label = PHASE_LABELS[entry.phase]?.[locale] ?? entry.text;
  return (
    <div className="flex items-center gap-2 px-4 py-2 text-sm text-muted-foreground">
      <Loader2 className="h-4 w-4 animate-spin" />
      <span>{label}</span>
    </div>
  );
}
