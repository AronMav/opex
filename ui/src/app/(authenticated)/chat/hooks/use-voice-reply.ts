// ── hooks/use-voice-reply.ts ─────────────────────────────────────────────────
// Speaks the agent's answer aloud when the turn was voice-initiated.
// Single-shot TTS: waits for the full reply, then synthesises and plays it
// as one audio blob — no per-sentence streaming.

import { useState, useRef, useCallback, useEffect, type Dispatch, type SetStateAction } from "react";
import { assertToken } from "@/lib/api";
import { useChatStore } from "@/stores/chat-store";
import { getLiveMessages, type MessageSource, type ChatMessage, type FilePart } from "@/stores/chat-types";

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

/** The latest assistant message in the live view (or null). */
function findLastAssistant(source: MessageSource): ChatMessage | null {
  const msgs = getLiveMessages(source);
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].role === "assistant") return msgs[i];
  }
  return null;
}

/** Concatenated text-part content of an assistant message. */
function assistantText(m: ChatMessage): string {
  let t = "";
  for (const p of m.parts) if (p.type === "text") t += (t ? "\n" : "") + p.text;
  return t;
}

function isUpstreamEmptyGarbage(text: string): boolean {
  return text.trimStart().startsWith("(Empty response:");
}

export interface UseVoiceReplyOptions {
  currentAgent: string;
  messageSource: MessageSource;
  isStreaming: boolean;
  setVoiceReplyActive: Dispatch<SetStateAction<boolean>>;
}

export interface UseVoiceReply {
  silenceVoiceTurn: () => void;
  primeTtsAudio: () => void;
  ttsPlaying: boolean;
  speakerRef: React.RefObject<{ idle: boolean } | null>;
}

// Minimal speaker-like object so ChatComposer's continuous re-arm effect still works.
interface SimpleSpeaker {
  idle: boolean;
}

export function useVoiceReply({
  currentAgent,
  messageSource,
  isStreaming,
  setVoiceReplyActive,
}: UseVoiceReplyOptions): UseVoiceReply {
  const voiceTurnPending = useChatStore((s) => s.agents[s.currentAgent]?.voiceTurnPending ?? false);
  const [ttsPlaying, setTtsPlaying] = useState(false);
  const ttsAudioRef = useRef<HTMLAudioElement | null>(null);
  const ttsUrlRef = useRef<string | null>(null);
  const speakerRef = useRef<SimpleSpeaker | null>(null);
  const takenOverUrlRef = useRef<string | null>(null);
  const playCleanupRef = useRef<(() => void) | null>(null);
  const ttsAbortRef = useRef<AbortController | null>(null);

  // Initialise the speaker-like object once.
  if (!speakerRef.current) speakerRef.current = { idle: true };

  const stopTts = useCallback(() => {
    const a = ttsAudioRef.current;
    if (a) {
      try { a.pause(); } catch { /* ignore */ }
      a.removeAttribute("src");
    }
    playCleanupRef.current?.();
    if (ttsUrlRef.current) {
      URL.revokeObjectURL(ttsUrlRef.current);
      ttsUrlRef.current = null;
    }
    ttsAbortRef.current?.abort();
    ttsAbortRef.current = null;
    setTtsPlaying(false);
    if (speakerRef.current) speakerRef.current.idle = true;
    setVoiceReplyActive(false);
  }, []);

  const silenceVoiceTurn = useCallback(() => {
    stopTts();
    useChatStore.getState().setVoiceTurnPending(false, currentAgent);
    takenOverUrlRef.current = null;
  }, [stopTts, currentAgent]);

  const getTtsEl = useCallback(() => {
    if (!ttsAudioRef.current) ttsAudioRef.current = new Audio();
    return ttsAudioRef.current;
  }, []);

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

  const playBlob = useCallback(
    (blob: Blob): Promise<void> => {
      const a = getTtsEl();
      playCleanupRef.current?.();
      return new Promise<void>((resolve) => {
        if (ttsUrlRef.current) URL.revokeObjectURL(ttsUrlRef.current);
        const url = URL.createObjectURL(blob);
        ttsUrlRef.current = url;
        let done = false;
        const finish = () => {
          if (done) return;
          done = true;
          a.removeEventListener("ended", finish);
          a.removeEventListener("error", finish);
          if (playCleanupRef.current === finish) playCleanupRef.current = null;
          resolve();
        };
        a.addEventListener("ended", finish);
        a.addEventListener("error", finish);
        playCleanupRef.current = finish;
        a.src = url;
        void a.play().catch(() => finish());
      });
    },
    [getTtsEl],
  );

  // Single-shot TTS: synthesise the full reply text as one request.
  const synthFullReply = useCallback(
    async (text: string, agent: string) => {
      if (!text.trim()) return;
      const controller = new AbortController();
      ttsAbortRef.current = controller;
      if (speakerRef.current) speakerRef.current.idle = false;
      setTtsPlaying(true);
      try {
        const resp = await fetch("/api/tts/synthesize?agent=" + encodeURIComponent(agent), {
          method: "POST",
          headers: {
            Authorization: `Bearer ${assertToken()}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ text }),
          signal: controller.signal,
        });
        if (resp.status === 409) {
          console.debug(`[tts] synthesis skipped — disabled for ${agent}`);
          return;
        }
        if (!resp.ok) return;
        const blob = await resp.blob();
        await playBlob(blob);
      } catch {
        // aborted or network error — skip
      } finally {
        if (speakerRef.current) speakerRef.current.idle = true;
        setTtsPlaying(false);
        setVoiceReplyActive(false);
        ttsAbortRef.current = null;
      }
    },
    [playBlob, setVoiceReplyActive],
  );

  // Agent-audio takeover: an audio/* file part on the voice turn's reply
  // (e.g. a synthesize_speech voice answer) supersedes TTS.
  const handleTakeover = useCallback((source: MessageSource) => {
    const last = findLastAssistant(source);
    if (!last) return;
    const audio = last.parts.find(
      (p): p is FilePart => p.type === "file" && p.mediaType.startsWith("audio"),
    );
    if (!audio || audio.url === takenOverUrlRef.current) return;
    takenOverUrlRef.current = audio.url;
    setVoiceReplyActive(true);
    if (speakerRef.current) speakerRef.current.idle = false;
    setTtsPlaying(true);
    const url = audio.url;
    const agentAtDispatch = currentAgent;
    void (async () => {
      try {
        const resp = await fetch(url);
        if (!resp.ok) return;
        const blob = await resp.blob();
        const st = useChatStore.getState();
        if (
          st.currentAgent !== agentAtDispatch ||
          !st.agents[agentAtDispatch]?.voiceTurnPending ||
          takenOverUrlRef.current !== url
        ) {
          return;
        }
        await playBlob(blob);
      } catch {
        /* best-effort */
      } finally {
        if (speakerRef.current) speakerRef.current.idle = true;
        setTtsPlaying(false);
        setVoiceReplyActive(false);
      }
    })();
  }, [currentAgent, playBlob, setVoiceReplyActive]);

  // New turn (rising edge of streaming): stop any leftover speech.
  // Initialize ref to the actual isStreaming value — a hard-coded false
  // causes a spurious rising-edge fire on remount-during-stream.
  const prevStreamingRisingRef = useRef(isStreaming);
  useEffect(() => {
    const was = prevStreamingRisingRef.current;
    prevStreamingRisingRef.current = isStreaming;
    if (!was && isStreaming) {
      stopTts();
      takenOverUrlRef.current = null;
      if (voiceTurnPending) setVoiceReplyActive(true);
    }
  }, [isStreaming, voiceTurnPending, stopTts, setVoiceReplyActive]);

  // Handle agent-audio takeover during streaming.
  useEffect(() => {
    if (!voiceTurnPending) return;
    handleTakeover(messageSource);
  }, [messageSource, voiceTurnPending, handleTakeover]);

  // Turn finished (falling edge): synthesise the full reply as one TTS request.
  const prevStreamingRef = useRef(false);
  useEffect(() => {
    const was = prevStreamingRef.current;
    prevStreamingRef.current = isStreaming;
    if (was && !isStreaming && voiceTurnPending) {
      const last = findLastAssistant(messageSource);
      const garbage = last ? isUpstreamEmptyGarbage(assistantText(last)) : false;
      if (!garbage && last) {
        const text = assistantText(last);
        void synthFullReply(text, currentAgent);
      }
      useChatStore.getState().setVoiceTurnPending(false, currentAgent);
      takenOverUrlRef.current = null;
    }
  }, [isStreaming, voiceTurnPending, messageSource, currentAgent, synthFullReply]);

  // Cleanup on unmount or agent change.
  useEffect(() => {
    return () => {
      stopTts();
      speakerRef.current = null;
    };
  }, [currentAgent, stopTts]);

  return { silenceVoiceTurn, primeTtsAudio, ttsPlaying, speakerRef };
}