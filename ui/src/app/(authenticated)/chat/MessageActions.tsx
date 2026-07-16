"use client";

import { useState, useCallback, useEffect, useRef } from "react";
import { useChatStore } from "@/stores/chat-store";
import { assertToken } from "@/lib/api";
import { copyText } from "@/lib/clipboard";
import { useProviderActive } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import type { ChatMessage, TextPart } from "@/stores/chat-store";
import { Button } from "@/components/ui/button";
import {
  Check,
  Copy,
  Download,
  RotateCcw,
  Volume2,
  VolumeX,
  ThumbsUp,
  ThumbsDown,
  Pencil,
  Trash2,
  MoreHorizontal,
} from "lucide-react";
import { Sheet, SheetContent, SheetTitle, SheetTrigger } from "@/components/ui/sheet";
import { toast } from "sonner";

// ── Helpers ─────────────────────────────────────────────────────────────────

// Ensure ≥44px touch target on mobile (WCAG 2.5.5) while keeping desktop's
// compact size-8 icon density (md:min-* resets the tap-target minimums).
const TOUCH_ICON = "tap-target md:min-h-0 md:min-w-0";

function extractText(message: ChatMessage): string {
  return message.parts
    .filter((p): p is TextPart => p.type === "text")
    .map((p) => p.text)
    .join("\n");
}

// ── Copy button ─────────────────────────────────────────────────────────────

function CopyButton({ message }: { message: ChatMessage }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    const text = extractText(message);
    copyText(text)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
      })
      .catch(() => toast.error(t("chat.copy_error")));
  }, [message, t]);

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      onClick={handleCopy}
      className={`rounded-full text-muted-foreground/50 hover:text-muted-foreground hover:bg-muted/50 ${TOUCH_ICON}`}
      title={t("chat.copy_tooltip")}
      aria-label={t("chat.copy_tooltip")}
    >
      {copied ? (
        <Check className="h-3.5 w-3.5 text-success" />
      ) : (
        <Copy className="h-3.5 w-3.5" />
      )}
    </Button>
  );
}

// ── Export markdown button ───────────────────────────────────────────────────

function ExportMarkdownButton({ message }: { message: ChatMessage }) {
  const { t } = useTranslation();

  const handleExport = useCallback(() => {
    const text = extractText(message);
    const blob = new Blob([text], { type: "text/markdown" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `message-${message.id}.md`;
    a.click();
    URL.revokeObjectURL(url);
  }, [message]);

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      onClick={handleExport}
      className={`rounded-full text-muted-foreground/50 hover:text-muted-foreground hover:bg-muted/50 ${TOUCH_ICON}`}
      title={t("chat.export_tooltip")}
      aria-label={t("chat.export_tooltip")}
    >
      <Download className="h-3.5 w-3.5" />
    </Button>
  );
}

// ── Reload/regenerate button ────────────────────────────────────────────────

function ReloadButton() {
  const { t } = useTranslation();

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      onClick={() => useChatStore.getState().regenerate()}
      className={`rounded-full text-muted-foreground/50 hover:text-muted-foreground hover:bg-muted/50 ${TOUCH_ICON}`}
      title={t("chat.regenerate_tooltip")}
      aria-label={t("chat.regenerate_tooltip")}
    >
      <RotateCcw className="h-3.5 w-3.5" />
    </Button>
  );
}

// ── Speak / stop speaking toggle ────────────────────────────────────────────

function SpeakButton({ message }: { message: ChatMessage }) {
  const { t } = useTranslation();
  const { data: active } = useProviderActive();
  const ttsAvailable = !!active?.find((r) => r.capability === "tts" && r.provider_name);
  const [playing, setPlaying] = useState(false);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const blobUrlRef = useRef<string | null>(null);

  const cleanup = useCallback(() => {
    if (audioRef.current) {
      audioRef.current.pause();
      audioRef.current = null;
    }
    if (blobUrlRef.current) {
      URL.revokeObjectURL(blobUrlRef.current);
      blobUrlRef.current = null;
    }
    setPlaying(false);
  }, []);

  useEffect(() => () => cleanup(), [cleanup]);

  const handleClick = useCallback(() => {
    if (playing) {
      cleanup();
      return;
    }

    const text = extractText(message);
    if (!text) return;

    setPlaying(true);

    (async () => {
      try {
        const resp = await fetch("/api/tts/synthesize", {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            Authorization: `Bearer ${assertToken()}`,
          },
          body: JSON.stringify({ text }),
        });
        if (!resp.ok) throw new Error(`TTS failed: ${resp.status}`);
        const blob = await resp.blob();
        const url = URL.createObjectURL(blob);
        blobUrlRef.current = url;
        const audio = new Audio(url);
        audioRef.current = audio;

        audio.addEventListener("ended", cleanup);
        audio.addEventListener("error", cleanup);
        audio.play();
      } catch {
        cleanup();
        toast.error(t("chat.tts_error"));
      }
    })();
  }, [message, playing, cleanup, t]);

  // Hide the button entirely when no TTS provider is configured — clicking
  // would only surface a 503. When provider-active query is still loading
  // (`active` is undefined) we default to rendering so the button doesn't
  // flicker in/out on page load.
  if (active && !ttsAvailable) return null;

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      onClick={handleClick}
      className={`rounded-full ${TOUCH_ICON} ${
        playing
          ? "text-destructive hover:text-destructive hover:bg-destructive/10"
          : "text-muted-foreground/50 hover:text-muted-foreground hover:bg-muted/50"
      }`}
      title={playing ? t("chat.stop_speaking_tooltip") : t("chat.speak_tooltip")}
      aria-label={playing ? t("chat.stop_speaking_tooltip") : t("chat.speak_tooltip")}
    >
      {playing ? <VolumeX className="h-3.5 w-3.5" /> : <Volume2 className="h-3.5 w-3.5" />}
    </Button>
  );
}

// ── Feedback buttons ────────────────────────────────────────────────────────

function FeedbackButtons({ message }: { message: ChatMessage }) {
  const { t } = useTranslation();
  const [submitted, setSubmitted] = useState<"positive" | "negative" | null>(null);
  const currentAgent = useChatStore((s) => s.currentAgent);

  const handleFeedback = useCallback(
    (type: "positive" | "negative") => {
      const feedback = type === "positive" ? 1 : -1;
      setSubmitted(type);
      // ?agent= is required server-side (audit 2026-05-08, IDOR fix); without
      // it the backend rejects the call to prevent cross-agent feedback writes.
      fetch(`/api/messages/${message.id}/feedback?agent=${encodeURIComponent(currentAgent)}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${assertToken()}`,
        },
        body: JSON.stringify({ feedback }),
      }).catch(() => toast.error(t("chat.feedback_error")));
    },
    [message.id, t, currentAgent],
  );

  return (
    <>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => handleFeedback("positive")}
        className={`rounded-full ${TOUCH_ICON} ${
          submitted === "positive"
            ? "text-success"
            : "text-muted-foreground/50 hover:text-success hover:bg-success/10"
        }`}
        title={t("chat.like_tooltip")}
        aria-label={t("chat.like_tooltip")}
      >
        <ThumbsUp className="h-3.5 w-3.5" />
      </Button>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => handleFeedback("negative")}
        className={`rounded-full ${TOUCH_ICON} ${
          submitted === "negative"
            ? "text-destructive"
            : "text-muted-foreground/50 hover:text-destructive hover:bg-destructive/10"
        }`}
        title={t("chat.dislike_tooltip")}
        aria-label={t("chat.dislike_tooltip")}
      >
        <ThumbsDown className="h-3.5 w-3.5" />
      </Button>
    </>
  );
}

// ── Edit button (user messages only) ────────────────────────────────────────

function EditButton({ onEdit }: { onEdit: () => void }) {
  const { t } = useTranslation();

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      data-action="edit"
      onClick={onEdit}
      className={`rounded-full text-muted-foreground/50 hover:text-muted-foreground hover:bg-muted/50 ${TOUCH_ICON}`}
      title={t("chat.edit_tooltip")}
      aria-label={t("chat.edit_tooltip")}
    >
      <Pencil className="h-3.5 w-3.5" />
    </Button>
  );
}

// ── Delete button ───────────────────────────────────────────────────────────

function DeleteMessageButton({ messageId }: { messageId: string }) {
  const { t } = useTranslation();
  const [deleteArmed, setDeleteArmed] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => () => clearTimeout(timerRef.current), []);

  const handleClick = useCallback(() => {
    if (deleteArmed) {
      useChatStore.getState().deleteMessage(messageId);
      setDeleteArmed(false);
    } else {
      setDeleteArmed(true);
      clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => setDeleteArmed(false), 3000);
    }
  }, [deleteArmed, messageId]);

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      onClick={handleClick}
      className={`rounded-full transition-colors ${TOUCH_ICON} ${
        deleteArmed
          ? "text-destructive bg-destructive/10"
          : "text-muted-foreground/50 hover:text-destructive hover:bg-destructive/10"
      }`}
      title={deleteArmed ? t("chat.delete_message_confirm") : t("chat.delete_message_tooltip")}
      aria-label={deleteArmed ? t("chat.delete_message_confirm") : t("chat.delete_message_tooltip")}
    >
      <Trash2 className="h-3.5 w-3.5" />
    </Button>
  );
}

// ── Main MessageActions component ───────────────────────────────────────────

const EMPTY_MESSAGE_SOURCE = { mode: "new-chat" as const };

export function MessageActions({
  message,
  showReload,
  onEdit,
}: {
  message: ChatMessage;
  showReload?: boolean;
  onEdit?: () => void;
}) {
  const { t } = useTranslation();
  const messageSource = useChatStore((s) => s.agents[s.currentAgent]?.messageSource ?? EMPTY_MESSAGE_SOURCE);

  return (
    <div className="flex items-center gap-0.5 md:opacity-0 md:group-hover:opacity-100 md:group-focus-within:opacity-100 focus-within:opacity-100 transition-opacity">
      <CopyButton message={message} />
      {showReload && (
        <>
          <ReloadButton />
          <div className="hidden md:flex items-center gap-0.5">
            <SpeakButton message={message} />
            <FeedbackButtons message={message} />
            <ExportMarkdownButton message={message} />
          </div>
          <div className="md:hidden">
            <Sheet>
              <SheetTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className={`rounded-full text-muted-foreground/50 ${TOUCH_ICON}`}
                  aria-label={t("chat.more_actions")}
                >
                  <MoreHorizontal className="h-3.5 w-3.5" />
                </Button>
              </SheetTrigger>
              <SheetContent side="bottom" className="rounded-t-xl px-4 pb-[max(2rem,env(safe-area-inset-bottom))]">
                <SheetTitle className="sr-only">{t("chat.more_actions")}</SheetTitle>
                <div className="flex items-center justify-around gap-2 pt-2">
                  <SpeakButton message={message} />
                  <FeedbackButtons message={message} />
                  <ExportMarkdownButton message={message} />
                </div>
              </SheetContent>
            </Sheet>
          </div>
        </>
      )}
      {!showReload && onEdit && <EditButton onEdit={onEdit} />}
      {!showReload && <ExportMarkdownButton message={message} />}
      {messageSource.mode === "history" && <DeleteMessageButton messageId={message.id} />}
    </div>
  );
}
