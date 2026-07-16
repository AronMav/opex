// ── hooks/use-voice-input.ts ─────────────────────────────────────────────────
// Voice INPUT wiring for the composer: the VAD-enabled recorder, the hands-free
// (continuous) config, the persisted sensitivity/pause tuning + its settings
// popover focus-trap, and the `handleAutoResult` dispatch that auto-sends (or
// queues) a transcript. Extracted VERBATIM from ChatComposer — nothing rewritten.
//
// The composer keeps `voiceReplyActive` (the "preparing" indicator) and passes
// `setVoiceReplyActive` in: `handleAutoResult` arms it on a direct voice submit.
// `formRef`/`textareaRef` are the composer's refs (used by the form + textarea in
// JSX), so they are passed in rather than owned here.

import {
  useState,
  useRef,
  useEffect,
  useMemo,
  useCallback,
  type Dispatch,
  type SetStateAction,
  type RefObject,
} from "react";
import { useChatStore } from "@/stores/chat-store";
import { useTranslation } from "@/hooks/use-translation";
import { useFocusTrap } from "@/hooks/use-focus-trap";
import { useVoiceRecorder, type UseVoiceRecorder } from "./use-voice-recorder";

export interface UseVoiceInputOptions {
  isStreaming: boolean;
  currentAgent: string;
  t: ReturnType<typeof useTranslation>["t"];
  formRef: RefObject<HTMLFormElement | null>;
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  /** Composer-owned "preparing" indicator setter — armed on a direct voice submit. */
  setVoiceReplyActive: Dispatch<SetStateAction<boolean>>;
}

export interface UseVoiceInput {
  voice: UseVoiceRecorder;
  continuous: boolean;
  setContinuous: Dispatch<SetStateAction<boolean>>;
  voiceSensitivity: number;
  setVoiceSensitivity: Dispatch<SetStateAction<number>>;
  voicePauseMs: number;
  setVoicePauseMs: Dispatch<SetStateAction<number>>;
  voiceSettingsOpen: boolean;
  setVoiceSettingsOpen: Dispatch<SetStateAction<boolean>>;
  voiceSettingsTriggerRef: RefObject<HTMLButtonElement | null>;
  voiceSettingsPanelRef: RefObject<HTMLDivElement | null>;
  voiceSettingsKeyDown: (e: React.KeyboardEvent) => void;
  closeVoiceSettings: () => void;
  insertTranscript: (text: string) => void;
}

export function useVoiceInput({
  isStreaming,
  currentAgent,
  t,
  formRef,
  textareaRef,
  setVoiceReplyActive,
}: UseVoiceInputOptions): UseVoiceInput {
  // ── Voice: VAD auto-stop + optional continuous (hands-free) ───────────────
  const [continuous, setContinuous] = useState(false);
  const continuousRef = useRef(false);
  useEffect(() => {
    continuousRef.current = continuous;
  }, [continuous]);
  const emptyCountRef = useRef(0);

  // Voice input tuning (persisted): sensitivity 0..100 (50 = current default),
  // pause 1000..5000ms before auto-stop on silence.
  const [voiceSensitivity, setVoiceSensitivity] = useState(50);
  const [voicePauseMs, setVoicePauseMs] = useState(2000);
  const [voiceSettingsOpen, setVoiceSettingsOpen] = useState(false);
  const voiceSettingsTriggerRef = useRef<HTMLButtonElement | null>(null);
  const voiceSettingsPanelRef = useRef<HTMLDivElement | null>(null);
  // Focus into the popover on open, restore to the gear on close, trap Tab.
  const voiceSettingsKeyDown = useFocusTrap({
    active: voiceSettingsOpen,
    containerRef: voiceSettingsPanelRef,
    restoreTo: voiceSettingsTriggerRef,
    initialFocus: "first",
  });
  const closeVoiceSettings = useCallback(() => setVoiceSettingsOpen(false), []);
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
        if (isStreaming) {
          // A turn is already running — queue instead of interrupting it (the
          // previous behavior lost the in-flight turn's work). Drains after the
          // turn via ChatThread; the drain arms voiceTurnPending so the reply
          // is still spoken. voice:true also appends (via "\n") if the user
          // speaks more than once during the same turn.
          useChatStore.getState().queueMessage(text, undefined, { voice: true });
          return;
        }
        insertTranscript(text);
        useChatStore.getState().setVoiceTurnPending(true, currentAgent);
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
    [insertTranscript, t, isStreaming, currentAgent],
  );

  const voice = useVoiceRecorder({ vad: true, vadConfig, onAutoResult: handleAutoResult });

  return {
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
  };
}
