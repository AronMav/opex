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
  X,
  Send,
  MoreHorizontal,
} from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "sonner";

// ── Helpers ─────────────────────────────────────────────────────────────────

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
      className="rounded-full text-muted-foreground/40 hover:text-muted-foreground hover:bg-muted/50"
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
      className="rounded-full text-muted-foreground/40 hover:text-muted-foreground hover:bg-muted/50"
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
      className="rounded-full text-muted-foreground/40 hover:text-muted-foreground hover:bg-muted/50"
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
      className={`rounded-full ${
        playing
          ? "text-destructive hover:text-destructive hover:bg-destructive/10"
          : "text-muted-foreground/40 hover:text-muted-foreground hover:bg-muted/50"
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

  const handleFeedback = useCallback(
    (type: "positive" | "negative") => {
      const feedback = type === "positive" ? 1 : -1;
      setSubmitted(type);
      fetch(`/api/messages/${message.id}/feedback`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${assertToken()}`,
        },
        body: JSON.stringify({ feedback }),
      }).catch(() => toast.error(t("chat.feedback_error")));
    },
    [message.id, t],
  );

  return (
    <>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={() => handleFeedback("positive")}
        className={`rounded-full ${
          submitted === "positive"
            ? "text-success"
            : "text-muted-foreground/40 hover:text-success hover:bg-success/10"
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
        className={`rounded-full ${
          submitted === "negative"
            ? "text-destructive"
            : "text-muted-foreground/40 hover:text-destructive hover:bg-destructive/10"
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

function EditButton({ message }: { message: ChatMessage }) {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [editText, setEditText] = useState("");

  const handleStartEdit = useCallback(() => {
    setEditText(extractText(message));
    setEditing(true);
  }, [message]);

  const handleCancel = useCallback(() => {
    setEditing(false);
    setEditText("");
  }, []);

  const handleSubmit = useCallback(() => {
    setEditing(false);
    setEditText("");
    useChatStore.getState().forkAndRegenerate(message.id, editText);
  }, [message.id, editText]);

  if (editing) {
    return (
      <div className="flex flex-col gap-2 w-full mt-2">
        <textarea
          value={editText}
          onChange={(e) => setEditText(e.target.value)}
          className="min-h-[80px] w-full resize-none rounded-lg border border-border bg-background px-3 py-2 text-sm text-foreground outline-none focus:border-primary/50"
          autoFocus
        />
        <div className="flex items-center gap-2 justify-end">
          <Button variant="ghost" size="xs" onClick={handleCancel}>
            <X className="h-3 w-3 mr-1" />
            {t("common.cancel")}
          </Button>
          <Button variant="ghost" size="xs" onClick={handleSubmit} className="text-primary">
            <Send className="h-3 w-3 mr-1" />
            {t("common.save")}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      onClick={handleStartEdit}
      className="rounded-full text-muted-foreground/40 hover:text-muted-foreground hover:bg-muted/50"
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
      className={`rounded-full transition-colors ${
        deleteArmed
          ? "text-destructive bg-destructive/15"
          : "text-muted-foreground/40 hover:text-destructive hover:bg-destructive/10"
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
}: {
  message: ChatMessage;
  showReload?: boolean;
}) {
  const { t } = useTranslation();
  const messageSource = useChatStore((s) => s.agents[s.currentAgent]?.messageSource ?? EMPTY_MESSAGE_SOURCE);

  return (
    <div className="flex items-center gap-0.5 md:opacity-0 md:group-hover:opacity-100 transition-opacity">
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
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="rounded-full text-muted-foreground/40"
                  aria-label={t("chat.more_actions")}
                >
                  <MoreHorizontal className="h-3.5 w-3.5" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <SpeakButton message={message} />
                <FeedbackButtons message={message} />
                <ExportMarkdownButton message={message} />
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </>
      )}
      {!showReload && <EditButton message={message} />}
      {!showReload && <ExportMarkdownButton message={message} />}
      {messageSource.mode === "history" && <DeleteMessageButton messageId={message.id} />}
    </div>
  );
}
