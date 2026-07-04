"use client";

import React, { useState, useCallback, useRef, useEffect, useMemo } from "react";
import { cn } from "@/lib/utils";
import { assertToken } from "@/lib/api";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { uuid, getLiveMessages, type MessageSource } from "@/stores/chat-types";
import { useTranslation } from "@/hooks/use-translation";
import { useAuthStore } from "@/stores/auth-store";
import { Button } from "@/components/ui/button";
import { SlashMenu } from "../parts/SlashMenu";
import { MentionAutocomplete } from "./MentionAutocomplete";
import { ModelDropdown } from "./ModelDropdown";
import { FileActionButtons } from "./FileActionButtons";
import { useVoiceRecorder } from "../hooks/use-voice-recorder";
import { useProviderActive } from "@/lib/queries";
import {
  Send,
  Square,
  Download,
  Paperclip,
  X,
  Loader2,
  Mic,
  Repeat,
  SlidersHorizontal,
  Volume2,
} from "lucide-react";

// ── Draft persistence helpers ─────────────────────────────────────────────────

const DRAFT_PREFIX = "opex.draft.";

export function saveDraft(agent: string, text: string) {
  if (text) localStorage.setItem(DRAFT_PREFIX + agent, text);
  else localStorage.removeItem(DRAFT_PREFIX + agent);
}

export function loadDraft(agent: string): string {
  return localStorage.getItem(DRAFT_PREFIX + agent) ?? "";
}

export function clearDraft(agent: string) {
  localStorage.removeItem(DRAFT_PREFIX + agent);
}

// ── Composer ──────────────────────────────────────────────────────────────────

const EMPTY_MESSAGE_SOURCE = { mode: "new-chat" as const };

/** Tiny silent WAV blob URL — played during a user gesture to unlock the audio
 *  element so later programmatic TTS playback isn't blocked by autoplay policy. */
function silentWavUrl(): string {
  const sampleRate = 8000;
  const n = 400; // ~0.05s
  const buf = new ArrayBuffer(44 + n);
  const v = new DataView(buf);
  const w = (o: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i));
  };
  w(0, "RIFF");
  v.setUint32(4, 36 + n, true);
  w(8, "WAVE");
  w(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, sampleRate, true);
  v.setUint32(28, sampleRate, true);
  v.setUint16(32, 1, true);
  v.setUint16(34, 8, true);
  w(36, "data");
  v.setUint32(40, n, true);
  for (let i = 0; i < n; i++) v.setUint8(44 + i, 128); // 8-bit silence
  return URL.createObjectURL(new Blob([buf], { type: "audio/wav" }));
}

/** Text of the most recent assistant message — used to speak voice replies. */
function lastAssistantSpokenText(source: MessageSource): string {
  const msgs = getLiveMessages(source);
  for (let i = msgs.length - 1; i >= 0; i--) {
    const m = msgs[i];
    if (m.role !== "assistant") continue;
    let txt = "";
    for (const p of m.parts) if (p.type === "text") txt += (txt ? "\n" : "") + p.text;
    return txt.trim();
  }
  return "";
}

/** URL of the most recent assistant message's audio file part (e.g. a
 *  synthesize_speech voice reply) — "" when the latest assistant reply has none.
 *  Lets us auto-play the voice the model produced itself instead of re-synthesising
 *  its text. */
function lastAssistantAudioUrl(source: MessageSource): string {
  const msgs = getLiveMessages(source);
  for (let i = msgs.length - 1; i >= 0; i--) {
    const m = msgs[i];
    if (m.role !== "assistant") continue;
    for (const p of m.parts) {
      if (p.type === "file" && p.mediaType.startsWith("audio")) return p.url;
    }
    return ""; // latest assistant reply carries no audio part
  }
  return "";
}

interface AttachmentEntry {
  id: string;
  name: string;
  file: File;
  uploadId: string; // upload ROW UUID (result.filename), used for /api/files/{uploadId}/...
  content: Array<{ type: string; data: string; mimeType: string; filename?: string }>;
}

export function ChatComposer() {
  const { t } = useTranslation();
  const currentAgent = useChatStore((s) => s.currentAgent);
  const agents = useAuthStore((s) => s.agents);
  const messageSource = useChatStore((s) => s.agents[s.currentAgent]?.messageSource ?? EMPTY_MESSAGE_SOURCE);
  const activeSessionId =
    "sessionId" in messageSource ? (messageSource as { mode: string; sessionId: string }).sessionId : null;
  const connectionPhase = useChatStore((s) => s.agents[s.currentAgent]?.connectionPhase ?? "idle");
  const isStreaming = isActivePhase(connectionPhase);
  const pendingMessage = useChatStore((s) => s.agents[s.currentAgent]?.pendingMessage ?? null);
  const hasMessages = messageSource.mode !== "new-chat";

  // ── Voice recorder ───────────────────────────────────────────────────────
  const { data: activeProviders } = useProviderActive();
  const hasSttProvider = useMemo(
    () => activeProviders?.some((p) => p.capability === "stt" && p.provider_name) ?? false,
    [activeProviders],
  );
  const [slashQuery, setSlashQuery] = useState<string | null>(null);
  const [mentionQuery, setMentionQuery] = useState<string | null>(null);
  const [activeMentionId, setActiveMentionId] = useState<string | null>(null);
  const [resolvedMention, setResolvedMention] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<AttachmentEntry[]>([]);
  const formRef = useRef<HTMLFormElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [hasInput, setHasInput] = useState(false);
  const [uploadingCount, setUploadingCount] = useState(0);
  const isUploading = uploadingCount > 0;

  // ── Voice: VAD auto-stop + optional continuous (hands-free) ───────────────
  const [continuous, setContinuous] = useState(false);
  const continuousRef = useRef(false);
  useEffect(() => {
    continuousRef.current = continuous;
  }, [continuous]);
  const emptyCountRef = useRef(0);

  // Voice reply: speak the agent's answer aloud when the turn was sent by voice.
  const voiceReplyPendingRef = useRef(false);
  const ttsPlayingRef = useRef(false);
  const [ttsPlaying, setTtsPlaying] = useState(false);
  // Drives the composer's voice-status indicator: true from a voice submit until
  // the spoken reply finishes (covers the slow synthesize_speech TTS synthesis,
  // when the chat is otherwise empty, plus playback).
  const [voiceReplyActive, setVoiceReplyActive] = useState(false);
  const ttsAudioRef = useRef<HTMLAudioElement | null>(null);
  const ttsUrlRef = useRef<string | null>(null);

  // Voice input tuning (persisted): sensitivity 0..100 (50 = current default),
  // pause 1000..5000ms before auto-stop on silence.
  const [voiceSensitivity, setVoiceSensitivity] = useState(50);
  const [voicePauseMs, setVoicePauseMs] = useState(2000);
  const [voiceSettingsOpen, setVoiceSettingsOpen] = useState(false);
  useEffect(() => {
    const s = Number(localStorage.getItem("opex.voice.sensitivity"));
    if (Number.isFinite(s) && s >= 0 && s <= 100) setVoiceSensitivity(s);
    const p = Number(localStorage.getItem("opex.voice.pauseMs"));
    if (Number.isFinite(p) && p >= 1000 && p <= 5000) setVoicePauseMs(p);
  }, []);
  useEffect(() => {
    localStorage.setItem("opex.voice.sensitivity", String(voiceSensitivity));
  }, [voiceSensitivity]);
  useEffect(() => {
    localStorage.setItem("opex.voice.pauseMs", String(voicePauseMs));
  }, [voicePauseMs]);
  // Higher sensitivity → lower threshold (picks up quieter speech). 50 keeps the
  // original tuning (thresholdFloorMult 3, thresholdMin 0.01).
  const vadConfig = useMemo(
    () => ({
      thresholdFloorMult: 5 - (voiceSensitivity / 100) * 4,
      thresholdMin: 0.018 - (voiceSensitivity / 100) * 0.016,
      silenceStopMs: voicePauseMs,
    }),
    [voiceSensitivity, voicePauseMs],
  );

  const insertTranscript = useCallback((text: string) => {
    const ta = textareaRef.current;
    if (!ta || !text) return;
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    const newVal = (ta.value ? ta.value + " " : "") + text;
    setter?.call(ta, newVal);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    ta.focus();
  }, []);

  // Called when VAD auto-stops with a transcript. The turn is auto-sent so the
  // agent actually replies (hands-free voice). Continuous mode additionally
  // re-arms recording after the reply (see the effect below).
  const handleAutoResult = useCallback(
    (text: string) => {
      if (text) {
        emptyCountRef.current = 0;
        insertTranscript(text);
        voiceReplyPendingRef.current = true;
        setVoiceReplyActive(true);
        formRef.current?.requestSubmit();
      } else if (continuousRef.current) {
        // Empty cycle (no speech). Stop hands-free after 3 in a row.
        emptyCountRef.current += 1;
        if (emptyCountRef.current >= 3) {
          setContinuous(false);
          void import("sonner").then(({ toast }) => toast.info(t("chat.voice_continuous_stopped")));
        }
      }
    },
    [insertTranscript, t],
  );

  const voice = useVoiceRecorder({ vad: true, vadConfig, onAutoResult: handleAutoResult });

  // ── Voice reply: speak the agent's answer (TTS playback) ──────────────────
  // One persistent <audio> element kept "unlocked" via primeTtsAudio() on a user
  // gesture, so the later (async) reply playback isn't blocked by autoplay policy.
  const stopTts = useCallback(() => {
    ttsAudioRef.current?.pause();
    if (ttsUrlRef.current) {
      URL.revokeObjectURL(ttsUrlRef.current);
      ttsUrlRef.current = null;
    }
    ttsPlayingRef.current = false;
    setTtsPlaying(false);
    setVoiceReplyActive(false);
  }, []);

  const getTtsEl = useCallback(() => {
    if (!ttsAudioRef.current) {
      const a = new Audio();
      a.addEventListener("ended", stopTts);
      a.addEventListener("error", stopTts);
      ttsAudioRef.current = a;
    }
    return ttsAudioRef.current;
  }, [stopTts]);

  useEffect(() => () => stopTts(), [stopTts]);

  // Unlock audio during a user gesture (mic tap / continuous toggle) by playing
  // a brief silent clip — later programmatic TTS plays on the same element pass.
  const primeTtsAudio = useCallback(() => {
    try {
      const a = getTtsEl();
      const u = silentWavUrl();
      a.src = u;
      const p = a.play();
      if (p && typeof p.then === "function") {
        p.then(() => {
          a.pause();
          a.currentTime = 0;
        })
          .catch(() => {})
          .finally(() => URL.revokeObjectURL(u));
      } else {
        URL.revokeObjectURL(u);
      }
    } catch {
      /* best-effort unlock */
    }
  }, [getTtsEl]);

  const playReply = useCallback(
    async (text: string) => {
      try {
        const resp = await fetch("/api/tts/synthesize", {
          method: "POST",
          headers: { "Content-Type": "application/json", Authorization: `Bearer ${assertToken()}` },
          body: JSON.stringify({ text }),
        });
        if (!resp.ok) throw new Error(`TTS ${resp.status}`);
        const blob = await resp.blob();
        if (ttsUrlRef.current) URL.revokeObjectURL(ttsUrlRef.current);
        const url = URL.createObjectURL(blob);
        ttsUrlRef.current = url;
        const a = getTtsEl();
        a.src = url;
        await a.play();
      } catch {
        stopTts();
        const { toast } = await import("sonner");
        toast.error(t("chat.tts_error"));
      }
    },
    [getTtsEl, stopTts, t],
  );

  // Play an already-synthesised audio URL (the model's synthesize_speech reply)
  // on the SAME pre-unlocked element playReply uses — a fresh `new Audio()` would
  // be blocked by the browser autoplay policy on the first hands-free reply.
  const playAudioUrl = useCallback(
    async (url: string) => {
      try {
        if (ttsUrlRef.current) {
          URL.revokeObjectURL(ttsUrlRef.current);
          ttsUrlRef.current = null;
        }
        const a = getTtsEl();
        a.src = url;
        await a.play();
      } catch {
        stopTts();
      }
    },
    [getTtsEl, stopTts],
  );

  // When a voice-initiated turn finishes streaming, speak the agent's reply.
  const prevStreamingRef = useRef(false);
  useEffect(() => {
    const was = prevStreamingRef.current;
    prevStreamingRef.current = isStreaming;
    if (was && !isStreaming && voiceReplyPendingRef.current) {
      voiceReplyPendingRef.current = false;
      // Prefer the voice the model produced itself (synthesize_speech audio part);
      // otherwise synthesise the reply TEXT. Mutually exclusive — synthesize_speech
      // ends the turn with empty text, so this never double-voices.
      const audioUrl = lastAssistantAudioUrl(messageSource);
      const text = audioUrl ? "" : lastAssistantSpokenText(messageSource);
      if (audioUrl || text) {
        ttsPlayingRef.current = true; // synchronous guard so continuous re-arm waits
        setTtsPlaying(true);
        if (audioUrl) void playAudioUrl(audioUrl);
        else void playReply(text);
      } else {
        setVoiceReplyActive(false); // nothing to voice — clear the indicator
      }
    }
  }, [isStreaming, messageSource, playReply, playAudioUrl]);

  // Continuous loop: re-arm recording once a turn finishes (idle, not streaming,
  // and not while the spoken reply is still playing — avoids recording the TTS).
  const voiceStartRef = useRef(voice.start);
  useEffect(() => {
    voiceStartRef.current = voice.start;
  });
  useEffect(() => {
    if (continuous && voice.state === "idle" && !isStreaming && !ttsPlayingRef.current) {
      void voiceStartRef.current();
    }
  }, [continuous, voice.state, isStreaming, ttsPlaying]);

  // Focus textarea on desktop only (avoid opening mobile keyboard on page load)
  useEffect(() => {
    if (window.innerWidth >= 1024) {
      textareaRef.current?.focus();
    }
  }, []);

  // Restore draft when mounting or switching agents
  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    const draft = loadDraft(currentAgent);
    if (draft) {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(ta, draft);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    } else {
      // Clear textarea when switching to an agent with no draft
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(ta, "");
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }, [currentAgent]);

  // Auto-resize textarea — use "0px" reset instead of "auto" to prevent flicker on paste
  const autoResize = useCallback(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    ta.style.height = "0px";
    ta.style.height = `${ta.scrollHeight}px`;
  }, []);

  const handleComposerInput = useCallback((e: React.FormEvent<HTMLFormElement>) => {
    const ta = e.target instanceof HTMLTextAreaElement ? e.target : null;
    if (!ta) return;
    setHasInput(!!ta.value.trim());
    saveDraft(currentAgent, ta.value);
    autoResize();
    const val = ta.value;
    if (val.startsWith("/") && !val.includes(" ") && !val.includes("\n") && !val.slice(1).includes("/")) {
      setSlashQuery(val);
      setMentionQuery(null);
    } else {
      setSlashQuery(null);
      // Detect @mention at end of input (preceded by whitespace or SOL)
      const match = val.match(/(?:^|\s)@(\w*)$/);
      setMentionQuery(match ? match[1] : null);
    }
    // Clear resolvedMention if @AgentName was removed from textarea
    setResolvedMention((prev) => {
      if (!prev) return null;
      const mentionPattern = new RegExp(`@${prev}\\b`);
      return mentionPattern.test(val) ? prev : null;
    });
  }, [autoResize, currentAgent]);

  const handleMentionSelect = useCallback((name: string) => {
    setMentionQuery(null);
    setResolvedMention(name);
    const ta = textareaRef.current;
    if (!ta) return;
    const val = ta.value;
    const match = val.match(/(?:^|\s)@(\w*)$/);
    if (match) {
      const before = val.slice(0, (match.index ?? 0) + (match[0].startsWith(" ") ? 1 : 0));
      const newVal = `${before}@${name} `;
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(ta, newVal);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
      ta.focus();
    }
  }, []);

  const clearResolvedMention = useCallback(() => {
    setResolvedMention(null);
    const ta = textareaRef.current;
    if (!ta) return;
    const val = ta.value;
    const cleaned = val.replace(/@\w+\s?/, "").trim();
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    setter?.call(ta, cleaned);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    ta.focus();
  }, []);

  const handleSlashSelect = useCallback((cmd: string) => {
    setSlashQuery(null);
    const ta = textareaRef.current;
    if (ta) {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(ta, "");
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    }
    const store = useChatStore.getState();
    if (cmd === "/stop")           { store.stopStream(); return; }
    if (cmd === "/new")            { store.newChat(); return; }
    if (cmd.startsWith("/think:")) { store.setThinkingLevel(parseInt(cmd.split(":")[1])); return; }
    // /reset and other commands are sent as messages — backend (engine_commands.rs) handles them
    store.sendMessage(cmd);
  }, []);

  const handleSlashClose = useCallback(() => {
    setSlashQuery(null);
  }, []);

  const handleFileAdd = useCallback(async (file: File) => {
    setUploadingCount(c => c + 1);
    try {
      const formData = new FormData();
      formData.append("file", file);
      const resp = await fetch("/api/media/upload", {
        method: "POST",
        headers: { Authorization: `Bearer ${assertToken()}` },
        body: formData,
      });
      if (!resp.ok) throw new Error(`Upload failed: ${resp.status}`);
      const result = await resp.json();
      // Use relative path (/uploads/uuid.ext) so the browser can load it without
      // TLS issues from public_url (e.g. https://192.168.1.85 has no valid cert).
      const uploadPath = (() => {
        try { return new URL(result.url as string).pathname; }
        catch { return result.url as string; }
      })();
      setAttachments((prev) => [
        ...prev,
        {
          id: uuid(),
          name: file.name,
          file,
          uploadId: result.filename as string, // R1: the row UUID, distinct from the served URL path
          content: [{ type: "file", data: uploadPath, mimeType: file.type, filename: file.name }],
        },
      ]);
    } catch (err) {
      const { toast } = await import("sonner");
      toast.error(`Upload failed: ${err instanceof Error ? err.message : "unknown error"}`);
    } finally {
      setUploadingCount(c => c - 1);
    }
  }, []);

  const handleSubmit = useCallback((e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const text = textareaRef.current?.value?.trim() ?? "";
    if (!text && attachments.length === 0) return;
    // sendMessage is now interrupt-aware: if streaming it calls interruptAndSend.
    useChatStore.getState().sendMessage(text, attachments);
    clearDraft(useChatStore.getState().currentAgent);
    setAttachments([]);
    setHasInput(false);
    if (textareaRef.current) {
      textareaRef.current.value = "";
      textareaRef.current.style.height = "auto";
    }
  }, [attachments]);

  const handleKeyDown = useCallback((e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // While the slash- or @-mention menu is open, Enter/Tab belong to the menu
    // (it selects the active item via its own capture-phase handler). Suppress
    // the textarea submit so a half-typed "/" or "@" is never sent. Unlike the
    // slash menu — which clears the textarea on select — the mention menu inserts
    // "@name " and leaves the text, so without this guard Enter would still send.
    if (slashQuery !== null || mentionQuery !== null) {
      if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); return; }
      if (e.key === "Tab") { e.preventDefault(); return; }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      // When streaming: form submit triggers sendMessage → interruptAndSend.
      formRef.current?.requestSubmit();
    } else if (e.key === "Enter" && e.shiftKey) {
      // Shift+Enter while idle: newline (default behavior, do nothing here).
      // Shift+Enter while streaming: queue the message instead of sending.
      const phase = useChatStore.getState().agents[useChatStore.getState().currentAgent]?.connectionPhase;
      if (isActivePhase(phase)) {
        e.preventDefault();
        const text = textareaRef.current?.value?.trim() ?? "";
        if (!text) return;
        useChatStore.getState().queueMessage(text, attachments.length > 0 ? attachments : undefined);
        clearDraft(useChatStore.getState().currentAgent);
        setAttachments([]);
        setHasInput(false);
        if (textareaRef.current) {
          textareaRef.current.value = "";
          textareaRef.current.style.height = "auto";
        }
      }
      // If idle: let default newline behavior proceed.
    }
  }, [attachments, slashQuery, mentionQuery]);

  const handleMentionClose = useCallback(() => {
    setMentionQuery(null);
  }, []);

  // ── Paste and drag-drop file attachment ──────────────────────────────────

  const [dragOver, setDragOver] = useState(false);

  const handlePaste = useCallback((e: React.ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (let i = 0; i < items.length; i++) {
      if (items[i].kind === "file") {
        e.preventDefault();
        const file = items[i].getAsFile();
        if (file) handleFileAdd(file);
        return; // handle first file only
      }
    }
    // If no files, let default paste behavior (text) proceed
  }, [handleFileAdd]);

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setDragOver(true);
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setDragOver(false);
  }, []);

  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setDragOver(false);
    const files = e.dataTransfer?.files;
    if (files && files.length > 0) {
      handleFileAdd(files[0]);
    }
  }, [handleFileAdd]);

  const handleMicClick = useCallback(async () => {
    if (voice.state === "recording") {
      // Manual stop: transcribe, insert, and auto-send so the agent replies.
      const text = await voice.stop();
      if (text) {
        insertTranscript(text);
        voiceReplyPendingRef.current = true;
        setVoiceReplyActive(true);
        formRef.current?.requestSubmit();
      }
    } else if (voice.state === "idle") {
      primeTtsAudio(); // unlock TTS playback while we still have the user gesture
      await voice.start();
    }
  }, [voice, insertTranscript, primeTtsAudio]);

  const formatElapsed = (secs: number): string => {
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return `${m}:${s.toString().padStart(2, "0")}`;
  };

  return (
    <div className="shrink-0 w-full p-3 md:p-4 pb-[max(0.75rem,env(safe-area-inset-bottom))] border-t border-border/50 bg-background/80 backdrop-blur-sm">
      <div className="mx-auto max-w-4xl">
        {(voiceReplyActive || ttsPlaying) && (
          <div
            role="status"
            aria-live="polite"
            className="mb-2 flex items-center gap-2 rounded-lg border border-primary/30 bg-primary/5 px-3 py-1.5 text-xs font-medium text-primary"
          >
            {ttsPlaying ? (
              <>
                <Volume2 className="h-3.5 w-3.5 animate-pulse" />
                {t("chat.voice_speaking")}
              </>
            ) : (
              <>
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                {t("chat.voice_preparing")}
              </>
            )}
          </div>
        )}
        <form
          ref={formRef}
          data-composer-input
          className={cn(
            "relative flex flex-col rounded-xl border bg-card/50 shadow-lg shadow-black/8 transition-all duration-200 focus-within:border-primary/50 focus-within:shadow-primary/8 focus-within:shadow-xl",
            dragOver ? "border-primary/50 bg-primary/5" : "border-border/50"
          )}
          onPaste={handlePaste}
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          onDrop={handleDrop}
          onInput={handleComposerInput}
          onSubmit={handleSubmit}
        >
          {dragOver && (
            <div className="absolute inset-0 z-20 flex items-center justify-center rounded-xl border-2 border-dashed border-primary/50 bg-primary/5 backdrop-blur-sm pointer-events-none">
              <div className="flex flex-col items-center gap-1 text-primary/80">
                <Paperclip className="h-6 w-6" />
                <span className="text-sm font-medium">{t("chat.drop_to_attach")}</span>
              </div>
            </div>
          )}
          {slashQuery !== null && (
            <SlashMenu
              query={slashQuery}
              onSelect={handleSlashSelect}
              onClose={handleSlashClose}
            />
          )}
          {mentionQuery !== null && agents.length > 1 && (
            <MentionAutocomplete
              query={mentionQuery}
              agents={agents.filter(a => a !== currentAgent)}
              onSelect={handleMentionSelect}
              onClose={handleMentionClose}
              onActiveChange={setActiveMentionId}
            />
          )}
          {attachments.length > 0 && attachments.map((att) => (
            <div key={att.id} className="flex flex-col">
              <div data-upload-id={att.uploadId} className="flex items-center gap-2 px-3 pt-2 text-xs text-muted-foreground">
                <Paperclip className="h-4 w-4" />
                <span className="truncate max-w-50">{att.name}</span>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("chat.remove_attachment")}
                  onClick={() => setAttachments((prev) => prev.filter((a) => a.id !== att.id))}
                >
                  <X size={12} />
                </Button>
              </div>
              <FileActionButtons
                uploadId={att.uploadId}
                mime={att.content[0]?.mimeType ?? att.file.type}
                agent={currentAgent}
                sessionId={activeSessionId}
              />
            </div>
          ))}
          {pendingMessage && (
            <div className="flex items-center gap-2 px-4 pt-2 pb-1 text-xs text-muted-foreground border-b border-border/30">
              <span className="flex-1 min-w-0 truncate">
                {t("chat.queue_prefix", { content: `${pendingMessage.content.slice(0, 60)}${pendingMessage.content.length > 60 ? "…" : ""}` })}
              </span>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("chat.cancel_queue")}
                onClick={() => useChatStore.getState().clearPending()}
              >
                <X size={12} />
              </Button>
            </div>
          )}
          <textarea
            ref={textareaRef}
            rows={1}
            enterKeyHint="send"
            autoCorrect="off"
            autoCapitalize="sentences"
            role={mentionQuery !== null ? "combobox" : undefined}
            aria-expanded={mentionQuery !== null ? true : undefined}
            aria-autocomplete={mentionQuery !== null ? "list" : undefined}
            aria-activedescendant={mentionQuery !== null ? (activeMentionId ?? undefined) : undefined}
            placeholder={
              messageSource.mode === "history"
                ? t("chat.continue_dialog")
                : t("chat.message_placeholder")
            }
            className="min-h-11 max-h-30 md:max-h-60 resize-none bg-transparent px-4 py-3 text-message text-foreground outline-none placeholder:text-muted-foreground/30"
            onKeyDown={handleKeyDown}
          />
          {resolvedMention && (
            <div data-testid="target-agent-indicator" className="flex items-center gap-1.5 px-4 py-1 text-xs text-muted-foreground">
              <span>Targeting</span>
              <span className="font-semibold text-primary">@{resolvedMention}</span>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("chat.clear_mention")}
                onClick={clearResolvedMention}
              >
                <X size={12} />
              </Button>
            </div>
          )}
          <div className="flex flex-wrap items-center justify-between px-3 pb-3">
            <div className="flex min-w-0 items-center gap-2">
              <input
                ref={fileInputRef}
                type="file"
                accept="image/*,audio/*,video/*,application/pdf,.txt,.md,.json,.csv"
                className="hidden"
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  if (file) handleFileAdd(file);
                  e.target.value = "";
                }}
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={t("chat.attach")}
                onClick={() => fileInputRef.current?.click()}
              >
                <Paperclip className="h-4 w-4" />
              </Button>
              {hasSttProvider && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  aria-label={
                    voice.state === "recording"
                      ? t("chat.stop_recording", { elapsed: formatElapsed(voice.elapsed) })
                      : t("chat.voice_input")
                  }
                  title={
                    voice.state === "recording"
                      ? t("chat.recording", { elapsed: formatElapsed(voice.elapsed) })
                      : voice.state === "transcribing"
                        ? t("chat.transcribing")
                        : t("chat.record_voice")
                  }
                  disabled={voice.state === "transcribing"}
                  onClick={handleMicClick}
                  className={cn(
                    "relative",
                    voice.state === "recording"
                      ? "text-destructive ring-2 ring-destructive/40 rounded-full"
                      : voice.state === "transcribing"
                        ? "text-muted-foreground/30 cursor-not-allowed"
                        : "text-muted-foreground/50 hover:text-muted-foreground",
                  )}
                >
                  {voice.state === "recording" && (
                    <span
                      aria-hidden
                      className="pointer-events-none absolute inset-0 rounded-full bg-destructive/30"
                      style={{
                        transform: `scale(${1 + Math.min(voice.level, 1) * 0.8})`,
                        opacity: Math.min(0.25 + voice.level * 2, 0.7),
                      }}
                    />
                  )}
                  {voice.state === "transcribing" ? (
                    <Loader2 className="relative h-4 w-4 animate-spin" />
                  ) : (
                    <Mic className="relative h-4 w-4" />
                  )}
                </Button>
              )}
              {hasSttProvider && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  aria-pressed={continuous}
                  aria-label={t("chat.continuous_voice")}
                  title={t("chat.continuous_voice")}
                  disabled={voice.state === "transcribing"}
                  onClick={() => {
                    if (!continuous) primeTtsAudio();
                    setContinuous((v) => !v);
                  }}
                  className={cn(
                    continuous
                      ? "text-primary bg-primary/10"
                      : "text-muted-foreground/50 hover:text-muted-foreground",
                  )}
                >
                  <Repeat className="h-4 w-4" />
                </Button>
              )}
              {hasSttProvider && (
                <div className="relative">
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    aria-label={t("chat.voice_settings")}
                    title={t("chat.voice_settings")}
                    onClick={() => setVoiceSettingsOpen((v) => !v)}
                    className={cn(
                      voiceSettingsOpen
                        ? "text-primary bg-primary/10"
                        : "text-muted-foreground/50 hover:text-muted-foreground",
                    )}
                  >
                    <SlidersHorizontal className="h-4 w-4" />
                  </Button>
                  {voiceSettingsOpen && (
                    <>
                      <div
                        className="fixed inset-0 z-40"
                        aria-hidden
                        onClick={() => setVoiceSettingsOpen(false)}
                      />
                      <div className="absolute bottom-full left-0 z-50 mb-2 w-64 rounded-lg border border-border/50 bg-card p-3 shadow-lg">
                        <div className="mb-3">
                          <div className="mb-1 flex items-center justify-between text-xs text-muted-foreground">
                            <span>{t("chat.voice_sensitivity")}</span>
                            <span className="font-mono">{voiceSensitivity}%</span>
                          </div>
                          <input
                            type="range"
                            min={0}
                            max={100}
                            step={5}
                            value={voiceSensitivity}
                            onChange={(e) => setVoiceSensitivity(Number(e.target.value))}
                            className="w-full accent-primary"
                          />
                        </div>
                        <div>
                          <div className="mb-1 flex items-center justify-between text-xs text-muted-foreground">
                            <span>{t("chat.voice_pause")}</span>
                            <span className="font-mono">{(voicePauseMs / 1000).toFixed(1)}s</span>
                          </div>
                          <input
                            type="range"
                            min={1000}
                            max={5000}
                            step={250}
                            value={voicePauseMs}
                            onChange={(e) => setVoicePauseMs(Number(e.target.value))}
                            className="w-full accent-primary"
                          />
                        </div>
                      </div>
                    </>
                  )}
                </div>
              )}
              <ModelDropdown agent={currentAgent} />
            </div>
            <div className="relative flex items-center gap-2">
              {hasMessages && !isStreaming && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  title={t("chat.export_session_tooltip")}
                  aria-label={t("chat.export_session")}
                  onClick={() => useChatStore.getState().exportSession()}
                >
                  <Download className="h-4 w-4" />
                </Button>
              )}
              {isStreaming && (
                <Button
                  type="button"
                  size="icon"
                  aria-label={t("chat.stop_and_keep")}
                  title={t("chat.stop_and_keep")}
                  onClick={() => useChatStore.getState().stopStream()}
                  className="h-11 w-11 md:h-10 md:w-10 rounded-xl border border-destructive/30 bg-destructive/10 text-destructive hover:bg-destructive/30 hover:border-destructive/50 shadow-sm animate-in fade-in zoom-in-90"
                >
                  <Square className="h-3.5 w-3.5 fill-current" />
                </Button>
              )}
              <Button
                type="submit"
                size="icon"
                aria-label={isStreaming ? t("chat.send_interrupt") : t("chat.send")}
                title={isStreaming ? t("chat.send_interrupt") : undefined}
                disabled={(!hasInput && attachments.length === 0) || isUploading}
                className="h-11 w-11 md:h-10 md:w-10 rounded-xl border border-primary/30 bg-primary/10 text-primary hover:bg-primary/20 hover:border-primary/50 shadow-sm disabled:opacity-30 disabled:shadow-none group/send animate-in fade-in zoom-in-90"
              >
                {isUploading
                  ? <Loader2 className="h-4 w-4 animate-spin" />
                  : <Send className="h-4 w-4 transition-transform duration-200 group-hover/send:translate-x-0.5 group-hover/send:-translate-y-0.5" />
                }
              </Button>
            </div>
          </div>
        </form>
      </div>
    </div>
  );
}
