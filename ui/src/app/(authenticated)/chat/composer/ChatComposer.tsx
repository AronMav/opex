"use client";

import React, { useState, useCallback, useRef, useEffect } from "react";
import { cn } from "@/lib/utils";
import { assertToken } from "@/lib/api";
import { useChatStore, isActivePhase } from "@/stores/chat-store";
import { uuid } from "@/stores/chat-types";
import { useTranslation } from "@/hooks/use-translation";
import { useAuthStore } from "@/stores/auth-store";
import { Button } from "@/components/ui/button";
import { SlashMenu } from "../parts/SlashMenu";
import { MentionAutocomplete } from "./MentionAutocomplete";
import { ModelDropdown } from "./ModelDropdown";
import {
  Send,
  Square,
  Download,
  Paperclip,
  X,
  Loader2,
} from "lucide-react";

// ── Draft persistence helpers ─────────────────────────────────────────────────

const DRAFT_PREFIX = "hydeclaw.draft.";

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

interface AttachmentEntry {
  id: string;
  name: string;
  file: File;
  content: Array<{ type: string; data: string; mimeType: string; filename?: string }>;
}

export function ChatComposer() {
  const { t } = useTranslation();
  const currentAgent = useChatStore((s) => s.currentAgent);
  const agents = useAuthStore((s) => s.agents);
  const messageSource = useChatStore((s) => s.agents[s.currentAgent]?.messageSource ?? EMPTY_MESSAGE_SOURCE);
  const connectionPhase = useChatStore((s) => s.agents[s.currentAgent]?.connectionPhase ?? "idle");
  const isStreaming = isActivePhase(connectionPhase);
  const hasMessages = messageSource.mode !== "new-chat";
  const [slashQuery, setSlashQuery] = useState<string | null>(null);
  const [mentionQuery, setMentionQuery] = useState<string | null>(null);
  const [resolvedMention, setResolvedMention] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<AttachmentEntry[]>([]);
  const formRef = useRef<HTMLFormElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [hasInput, setHasInput] = useState(false);
  const [uploadingCount, setUploadingCount] = useState(0);
  const isUploading = uploadingCount > 0;

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
  }, [currentAgent]);

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
      setAttachments((prev) => [
        ...prev,
        {
          id: uuid(),
          name: file.name,
          file,
          content: [{ type: "file", data: result.url as string, mimeType: file.type, filename: file.name }],
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
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      formRef.current?.requestSubmit();
    }
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

  return (
    <div className="shrink-0 w-full p-3 md:p-4 border-t border-border/50 bg-background/80 backdrop-blur-sm">
      <div className="mx-auto max-w-4xl">
        <form
          ref={formRef}
          data-composer-input
          className={cn(
            "relative flex flex-col rounded-xl border bg-card/90 shadow-lg shadow-black/8 transition-all duration-200 focus-within:border-primary/50 focus-within:shadow-primary/8 focus-within:shadow-xl",
            dragOver ? "border-primary/70 bg-primary/5" : "border-border/50"
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
              <div className="flex flex-col items-center gap-1 text-primary/70">
                <Paperclip className="h-6 w-6" />
                <span className="text-sm font-medium">Drop file to attach</span>
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
            />
          )}
          {attachments.length > 0 && attachments.map((att) => (
            <div key={att.id} className="flex items-center gap-2 px-3 pt-2 text-xs text-muted-foreground">
              <Paperclip className="h-3 w-3" />
              <span className="truncate max-w-[200px]">{att.name}</span>
              <button
                type="button"
                aria-label={t("chat.remove_attachment")}
                onClick={() => setAttachments((prev) => prev.filter((a) => a.id !== att.id))}
                className="rounded p-0.5 hover:bg-muted/50 text-muted-foreground/60 hover:text-muted-foreground transition-colors"
              >
                <X size={12} />
              </button>
            </div>
          ))}
          <textarea
            ref={textareaRef}
            rows={1}
            enterKeyHint="send"
            autoCorrect="off"
            autoCapitalize="sentences"
            placeholder={
              messageSource.mode === "history"
                ? t("chat.continue_dialog")
                : t("chat.message_placeholder")
            }
            className="min-h-[44px] max-h-[120px] md:max-h-[240px] resize-none bg-transparent px-4 py-3 text-[15px] text-foreground outline-none placeholder:text-muted-foreground/35"
            onKeyDown={handleKeyDown}
          />
          {resolvedMention && (
            <div data-testid="target-agent-indicator" className="flex items-center gap-1.5 px-4 py-1 text-xs text-muted-foreground">
              <span>Targeting</span>
              <span className="font-semibold text-primary">@{resolvedMention}</span>
              <button
                type="button"
                aria-label={t("chat.clear_mention")}
                onClick={clearResolvedMention}
                className="rounded p-0.5 hover:bg-muted/50 text-muted-foreground/60 hover:text-muted-foreground transition-colors"
              >
                <X size={12} />
              </button>
            </div>
          )}
          <div className="flex items-center justify-between px-3 pb-3">
            <div className="flex items-center gap-2">
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
              <button
                type="button"
                aria-label={t("chat.attach")}
                className="rounded p-3 md:p-2 text-muted-foreground/50 hover:text-muted-foreground hover:bg-muted/50 transition-colors"
                onClick={() => fileInputRef.current?.click()}
              >
                <Paperclip className="h-4 w-4" />
              </button>
              {agents.length > 1 && (
                <span className="font-mono text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/50 bg-muted/30 px-2 py-0.5 rounded">
                  {currentAgent}
                </span>
              )}
              <ModelDropdown agent={currentAgent} />
            </div>
            <div className="relative flex items-center gap-2">
              {hasMessages && !isStreaming && (
                <button
                  type="button"
                  title={t("chat.export_session_tooltip")}
                  aria-label={t("chat.export_session")}
                  className="rounded p-3 md:p-2 text-muted-foreground/40 hover:text-muted-foreground hover:bg-muted/50 transition-colors"
                  onClick={() => useChatStore.getState().exportSession()}
                >
                  <Download className="h-4 w-4" />
                </button>
              )}
              {isStreaming && (
                <Button
                  type="button"
                  size="icon"
                  aria-label={t("chat.slash_stop")}
                  onClick={() => useChatStore.getState().stopStream()}
                  className="h-11 w-11 md:h-10 md:w-10 rounded-xl border border-destructive/30 bg-destructive/15 text-destructive hover:bg-destructive/25 hover:border-destructive/50 shadow-sm animate-in fade-in zoom-in-90"
                >
                  <Square className="h-3.5 w-3.5 fill-current" />
                </Button>
              )}
              <Button
                type="submit"
                size="icon"
                aria-label={t("chat.send")}
                disabled={(!hasInput && attachments.length === 0) || isUploading || isStreaming}
                className="h-11 w-11 md:h-10 md:w-10 rounded-xl border border-primary/30 bg-primary/15 text-primary hover:bg-primary/25 hover:border-primary/50 shadow-sm disabled:opacity-30 disabled:shadow-none group/send animate-in fade-in zoom-in-90"
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
