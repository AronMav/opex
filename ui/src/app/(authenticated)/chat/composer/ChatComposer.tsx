"use client";

import React, { useState, useCallback, useRef, useEffect, useMemo, useId } from "react";
import { cn } from "@/lib/utils";
import { assertToken } from "@/lib/api";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { uuid } from "@/stores/chat-types";
import { useTranslation } from "@/hooks/use-translation";
import { useAuthStore } from "@/stores/auth-store";
import { Button } from "@/components/ui/button";
import { MentionAutocomplete } from "./MentionAutocomplete";
import { CommandAutocomplete, type AutocompleteItem } from "@/components/chat/command-autocomplete";
import { ModelDropdown } from "./ModelDropdown";
import { ImageLightbox } from "@/components/chat/ImageLightbox";
import { useVoiceInput } from "../hooks/use-voice-input";
import { useVoiceReply } from "../hooks/use-voice-reply";
import { useAgents } from "@/lib/queries";
import { useCommands } from "@/hooks/use-commands";
import { usePrompts } from "@/lib/prompts";
import { saveDraft, loadDraft, clearDraft } from "./draft";
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
// Live in ./draft (pure, React-free) so lightweight callers can reuse them;
// re-exported here to keep existing import sites working.
export { saveDraft, loadDraft, clearDraft } from "./draft";

// Maps the /think <level> word form to the numeric level accepted by
// setThinkingLevel — replaces the old /think:N colon syntax.
const THINK_LEVELS: Record<string, number> = {
  off: 0, minimal: 1, low: 2, medium: 3, high: 4, max: 5,
};

// ── Composer ──────────────────────────────────────────────────────────────────

const EMPTY_MESSAGE_SOURCE = { mode: "new-chat" as const };

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

  // ── Slash-command registry (server-backed autocomplete) ───────────────────
  // CommandAutocomplete is the single slash menu, driven entirely by the
  // /api/commands registry — no hardcoded command list.
  const { data: registryCommands } = useCommands(currentAgent);

  // ── Workspace prompt library (workspace/prompts.md) ───────────────────────
  // Renders as a "Prompts" section below matching commands in the same slash
  // menu — picking one replaces the composer text with the prompt body
  // (a starting template) instead of running/inserting a command.
  const { prompts } = usePrompts();

  // ── Voice recorder ───────────────────────────────────────────────────────
  // Gate voice controls on the CURRENT AGENT's capabilities (not provider_active,
  // which the Profiles project narrowed to embedding-only — leaving the mic
  // permanently hidden if left as-is). hasStt gates the mic (transcription works
  // standalone); hasTts additionally gates hands-free + voice-settings (those
  // depend on spoken replies).
  const { data: agentList } = useAgents();
  const currentAgentInfo = useMemo(
    () => agentList?.find((a) => a.name === currentAgent),
    [agentList, currentAgent],
  );
  const hasStt = currentAgentInfo?.capabilities?.stt ?? false;
  const hasTts = currentAgentInfo?.capabilities?.tts ?? false;
  const [slashQuery, setSlashQuery] = useState<string | null>(null);
  const [activeCommandId, setActiveCommandId] = useState<string | null>(null);
  const commandListboxId = useId();
  const [mentionQuery, setMentionQuery] = useState<string | null>(null);
  const [activeMentionId, setActiveMentionId] = useState<string | null>(null);
  const mentionListboxId = useId();
  const [resolvedMention, setResolvedMention] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<AttachmentEntry[]>([]);
  const formRef = useRef<HTMLFormElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [hasInput, setHasInput] = useState(false);
  const [composerText, setComposerText] = useState("");
  const [uploadingCount, setUploadingCount] = useState(0);
  const isUploading = uploadingCount > 0;

  // "preparing" indicator: true from a voice submit until the reply either starts
  // speaking, finishes with nothing to say, or is superseded/drained. Owned here
  // (composer) because it is set from three places — this component's mic handler,
  // useVoiceInput's auto-result, and useVoiceReply's effects — and read by the JSX.
  const [voiceReplyActive, setVoiceReplyActive] = useState(false);

  // Voice input: VAD-enabled recorder, hands-free (continuous) config, and the
  // persisted sensitivity/pause tuning + settings popover. Extracted VERBATIM to
  // use-voice-input; `setVoiceReplyActive` is passed in so its handleAutoResult can
  // arm the "preparing" indicator on a direct voice submit.
  const {
    voice,
    continuous,
    setContinuous,
    voiceSensitivity,
    setVoiceSensitivity,
    voicePauseMs,
    setVoicePauseMs,
    voiceSettingsOpen,
    setVoiceSettingsOpen,
    voiceSettingsTriggerRef,
    voiceSettingsPanelRef,
    voiceSettingsKeyDown,
    closeVoiceSettings,
    insertTranscript,
  } = useVoiceInput({ isStreaming, currentAgent, t, formRef, textareaRef, setVoiceReplyActive });

  // Voice reply (TTS playback of a voice-initiated turn): speaker pipeline,
  // rising/falling-edge streaming effects, and the Stop/`/stop` silencing helper.
  // Extracted VERBATIM to use-voice-reply — see that hook for the (fragile) effect
  // chain. `voiceReplyActive` stays composer-owned (set from three places); the
  // continuous re-arm effect below reads the returned `speakerRef`/`ttsPlaying`.
  const { silenceVoiceTurn, primeTtsAudio, ttsPlaying, speakerRef } = useVoiceReply({
    currentAgent,
    messageSource,
    isStreaming,
    setVoiceReplyActive,
  });

  // Continuous loop: re-arm recording once a turn finishes (idle, not streaming,
  // and not while the spoken reply is still playing — avoids recording the TTS).
  const voiceStartRef = useRef(voice.start);
  useEffect(() => {
    voiceStartRef.current = voice.start;
  });
  useEffect(() => {
    // Re-arm only once the speaker queue has fully drained — the mic must not
    // reopen while a spoken reply is still synthesising or playing (it would
    // record the TTS). `ttsPlaying` is the reactive trigger; speaker.idle is the
    // authoritative gate.
    const speakerIdle = speakerRef.current?.idle ?? true;
    if (continuous && voice.state === "idle" && !isStreaming && speakerIdle) {
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

  // H11 fix: stop any in-progress voice recording when the user switches
  // agents. The recorder state is keyed by `currentAgent` (auto-result +
  // transcript both route to the current agent's composer), so a recording
  // started on agent A would otherwise land its transcript on agent B after
  // the switch — confusing and irreversible. Stopping here drops any partial
  // audio without sending a transcript; the user can re-record on the new
  // agent if they wish.
  const prevAgentRef = useRef(currentAgent);
  useEffect(() => {
    if (prevAgentRef.current !== currentAgent) {
      prevAgentRef.current = currentAgent;
      if (voice.state === "recording" || voice.state === "transcribing") {
        // Fire-and-forget — the returned transcript is intentionally
        // discarded on an agent-switch abort.
        void voice.stop();
      }
    }
  }, [currentAgent, voice]);

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
    setComposerText(ta.value);
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

  // Client-side shortcut for slash commands — the single place that handles
  // /stop, /new and /think locally instead of round-tripping to the backend.
  // Used both by the normal submit flow (typed commands) and by
  // handleCommandPick (immediate no-arg registry picks), so the two paths
  // never diverge. Everything else (/reset, /compact, /status, /memory ...,
  // /rollback ...) is sent as a plain message — the backend (engine_commands.rs)
  // handles those.
  const dispatchSlashCommand = useCallback((text: string) => {
    const trimmed = text.trim();
    const store = useChatStore.getState();
    const spaceIdx = trimmed.indexOf(" ");
    const word = (spaceIdx === -1 ? trimmed : trimmed.slice(0, spaceIdx)).toLowerCase();
    const rest = (spaceIdx === -1 ? "" : trimmed.slice(spaceIdx + 1)).trim().toLowerCase();
    if (word === "/stop") { silenceVoiceTurn(); store.stopStream(); return; }
    if (word === "/new")  { store.newChat(); return; }
    if (word === "/think") {
      const level = /^[0-5]$/.test(rest) ? Number(rest) : THINK_LEVELS[rest];
      if (level !== undefined) { store.setThinkingLevel(level); return; }
    }
    store.sendMessage(trimmed);
  }, [silenceVoiceTurn]);

  const handleSlashClose = useCallback(() => {
    setSlashQuery(null);
  }, []);

  const clearComposerText = useCallback(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    setter?.call(ta, "");
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  }, []);

  // Registry-backed pick. No-arg commands (e.g. /new, /status) execute
  // immediately via dispatchSlashCommand — the same one-click UX the old
  // hardcoded menu gave for its fixed no-arg commands. Commands with args
  // insert "/name " and leave it for the user to fill in + Enter. Prompt
  // picks are a different kind entirely: they REPLACE the whole composer
  // text with the prompt body (a starting template) and never auto-send —
  // the user edits/sends it like anything else they typed.
  const handleAutocompletePick = useCallback((item: AutocompleteItem) => {
    setSlashQuery(null);
    if (item.kind === "prompt") {
      const ta = textareaRef.current;
      if (!ta) return;
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      setter?.call(ta, item.body);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
      ta.focus();
      ta.setSelectionRange(item.body.length, item.body.length);
      return;
    }
    const cmd = registryCommands?.find((c) => c.name === item.name);
    if (cmd && cmd.args.length === 0) {
      clearComposerText();
      dispatchSlashCommand(`/${item.name}`);
      return;
    }
    const ta = textareaRef.current;
    if (!ta) return;
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    setter?.call(ta, `/${item.name} `);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    ta.focus();
  }, [registryCommands, dispatchSlashCommand, clearComposerText]);

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
    if (attachments.length === 0 && text.startsWith("/")) {
      // Typed slash command: client shortcuts (/stop, /new, /think) run locally,
      // everything else is routed to sendMessage inside dispatchSlashCommand.
      dispatchSlashCommand(text);
    } else {
      // sendMessage is now interrupt-aware: if streaming it calls interruptAndSend.
      useChatStore.getState().sendMessage(text, attachments);
    }
    clearDraft(useChatStore.getState().currentAgent);
    setAttachments([]);
    setHasInput(false);
    setComposerText("");
    if (textareaRef.current) {
      textareaRef.current.value = "";
      textareaRef.current.style.height = "auto";
    }
  }, [attachments, dispatchSlashCommand]);

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
        setComposerText("");
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
    const files: File[] = [];
    for (let i = 0; i < items.length; i++) {
      if (items[i].kind === "file") {
        const file = items[i].getAsFile();
        if (file) files.push(file);
      }
    }
    if (files.length === 0) return; // no files → let default text paste proceed
    e.preventDefault();
    for (const file of files) handleFileAdd(file);
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
    if (!files) return;
    for (let i = 0; i < files.length; i++) {
      handleFileAdd(files[i]);
    }
  }, [handleFileAdd]);

  const handleMicClick = useCallback(async () => {
    if (voice.state === "recording") {
      // Manual stop: transcribe, insert, and auto-send so the agent replies.
      const text = await voice.stop();
      if (text) {
        if (isStreaming) {
          // Same reasoning as handleAutoResult: don't interrupt the running
          // turn, queue instead and let the drain arm the spoken reply.
          useChatStore.getState().queueMessage(text, undefined, { voice: true });
          return;
        }
        insertTranscript(text);
        useChatStore.getState().setVoiceTurnPending(true, currentAgent);
        setVoiceReplyActive(true);
        formRef.current?.requestSubmit();
      }
    } else if (voice.state === "idle") {
      primeTtsAudio(); // unlock TTS playback while we still have the user gesture
      await voice.start();
    }
  }, [voice, insertTranscript, primeTtsAudio, isStreaming, currentAgent]);

  const formatElapsed = (secs: number): string => {
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return `${m}:${s.toString().padStart(2, "0")}`;
  };

  return (
    <div className="shrink-0 w-full p-3 md:p-4 pb-[max(0.75rem,env(safe-area-inset-bottom))] border-t border-border/50 bg-background/80 backdrop-blur-sm">
      <div className="mx-auto max-w-4xl">
        {(pendingMessage?.voice || voiceReplyActive || ttsPlaying) && (
          <div
            role="status"
            aria-live="polite"
            className="mb-2 flex items-center gap-2 rounded-lg border border-primary/30 bg-primary/5 px-3 py-1.5 text-xs font-medium text-primary"
          >
            {pendingMessage?.voice ? (
              <>
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                {t("chat.voice_queued")}
              </>
            ) : ttsPlaying ? (
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
          data-composer-root
          className={cn(
            "relative flex flex-col rounded-xl border bg-card/50 shadow-elev-2 transition-all duration-200 focus-within:border-primary/50",
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
            <div className="absolute inset-0 z-[var(--z-overlay)] flex items-center justify-center rounded-xl border-2 border-dashed border-primary/50 bg-primary/5 backdrop-blur-sm pointer-events-none">
              <div className="flex flex-col items-center gap-1 text-primary/80">
                <Paperclip className="h-6 w-6" />
                <span className="text-sm font-medium">{t("chat.drop_to_attach")}</span>
              </div>
            </div>
          )}
          {slashQuery !== null && (
            <CommandAutocomplete
              input={slashQuery}
              commands={registryCommands ?? []}
              prompts={prompts}
              onPick={handleAutocompletePick}
              onClose={handleSlashClose}
              onActiveChange={setActiveCommandId}
              listboxId={commandListboxId}
            />
          )}
          {mentionQuery !== null && agents.length > 1 && (
            <MentionAutocomplete
              query={mentionQuery}
              agents={agents.filter(a => a !== currentAgent)}
              onSelect={handleMentionSelect}
              onClose={handleMentionClose}
              onActiveChange={setActiveMentionId}
              listboxId={mentionListboxId}
            />
          )}
          {attachments.length > 0 && attachments.map((att) => {
            const imageContent = att.content.find((c) => c.mimeType.startsWith("image/"));
            return (
              <div key={att.id} className="flex flex-col">
                <div data-upload-id={att.uploadId} className="flex items-center gap-2 px-3 pt-2 text-xs text-muted-foreground">
                  {imageContent ? (
                    <ImageLightbox src={imageContent.data} alt={att.name} className="h-12 w-12 rounded object-cover" />
                  ) : (
                    <Paperclip className="h-4 w-4" />
                  )}
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
              </div>
            );
          })}
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
            aria-label={t("chat.message_placeholder")}
            role={mentionQuery !== null || slashQuery !== null ? "combobox" : undefined}
            aria-expanded={mentionQuery !== null || slashQuery !== null ? true : undefined}
            aria-controls={mentionQuery !== null ? mentionListboxId : slashQuery !== null ? commandListboxId : undefined}
            aria-autocomplete={mentionQuery !== null || slashQuery !== null ? "list" : undefined}
            aria-activedescendant={mentionQuery !== null ? (activeMentionId ?? undefined) : slashQuery !== null ? (activeCommandId ?? undefined) : undefined}
            placeholder={
              messageSource.mode === "history"
                ? t("chat.continue_dialog")
                : t("chat.message_placeholder")
            }
            className="min-h-11 max-h-30 md:max-h-60 resize-none bg-transparent px-4 py-3 text-message text-foreground outline-none placeholder:text-muted-foreground"
            onKeyDown={handleKeyDown}
          />
          {resolvedMention && (
            <div data-testid="target-agent-indicator" className="flex items-center gap-1.5 px-4 py-1 text-xs text-muted-foreground">
              <span>{t("chat.mention_targeting")}</span>
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
            <div className="flex min-w-0 items-center gap-1 sm:gap-2">
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
              {hasStt && (
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
                        ? "text-muted-foreground-subtle cursor-not-allowed"
                        : "text-muted-foreground-subtle hover:text-muted-foreground",
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
              {hasStt && hasTts && (
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
                      : "text-muted-foreground-subtle hover:text-muted-foreground",
                  )}
                >
                  <Repeat className="h-4 w-4" />
                </Button>
              )}
              {hasStt && hasTts && (
                <div className="relative hidden sm:block">
                  <Button
                    ref={voiceSettingsTriggerRef}
                    type="button"
                    variant="ghost"
                    size="icon"
                    aria-label={t("chat.voice_settings")}
                    title={t("chat.voice_settings")}
                    aria-expanded={voiceSettingsOpen}
                    onClick={() => setVoiceSettingsOpen((v) => !v)}
                    className={cn(
                      voiceSettingsOpen
                        ? "text-primary bg-primary/10"
                        : "text-muted-foreground-subtle hover:text-muted-foreground",
                    )}
                  >
                    <SlidersHorizontal className="h-4 w-4" />
                  </Button>
                  {voiceSettingsOpen && (
                    <>
                      <div
                        className="fixed inset-0 z-40"
                        aria-hidden
                        onClick={closeVoiceSettings}
                      />
                      <div
                        ref={voiceSettingsPanelRef}
                        role="dialog"
                        aria-label={t("chat.voice_settings")}
                        onKeyDown={(e) => {
                          if (e.key === "Escape") {
                            e.stopPropagation();
                            closeVoiceSettings();
                            return;
                          }
                          voiceSettingsKeyDown(e);
                        }}
                        className="absolute bottom-full left-0 z-50 mb-2 w-64 rounded-lg border border-border/50 bg-card p-3 shadow-lg"
                      >
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
                  onClick={() => { silenceVoiceTurn(); useChatStore.getState().stopStream(); }}
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
