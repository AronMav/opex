// ── hooks/use-voice-reply.ts ─────────────────────────────────────────────────
// Speaks the agent's answer aloud when the turn was voice-initiated (TTS reply
// loop). Owns the streaming per-sentence TTS pipeline (SpeakerQueue + splitter),
// the single pre-unlocked <audio> element, and the voiceTurnPending rising /
// falling-edge effects that drive it. Extracted VERBATIM from ChatComposer —
// the effect chain is confirmed-fragile (two Important bugs were fixed here), so
// nothing is rewritten and the relative order of every effect is preserved.
//
// `voiceReplyActive` (the "preparing" indicator) is NOT owned here: it is set by
// both this hook's effects AND the composer's mic handler / voice-input's
// auto-result, so the composer owns that useState and passes `setVoiceReplyActive`
// down. `ttsPlaying` and `speakerRef` are owned here and returned — the composer's
// continuous re-arm effect reads them.

import { useState, useRef, useCallback, useEffect, type Dispatch, type SetStateAction } from "react";
import { assertToken } from "@/lib/api";
import { useChatStore } from "@/stores/chat-store";
import { getLiveMessages, type MessageSource, type ChatMessage, type FilePart } from "@/stores/chat-types";
import {
  createSentenceSplitter,
  createSpeakerQueue,
  type SentenceSplitter,
  type SpeakerQueue,
} from "./tts-speaker";

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

/** Concatenated text-part content of an assistant message — thinking / tool
 *  parts are excluded so only spoken prose reaches the TTS splitter. */
function assistantText(m: ChatMessage): string {
  let t = "";
  for (const p of m.parts) if (p.type === "text") t += (t ? "\n" : "") + p.text;
  return t;
}

// Mirrors the core-side is_upstream_empty_garbage: some upstream proxies
// serialize a thinking-only/empty LLM response as a literal "(Empty response: …)"
// text blob. The core blanks it for persistence, but it still streams as
// text-delta — so guard the voice feed here too, or it gets read aloud.
// Start-only match (like the server): a normal reply that merely CONTAINS the
// marker mid-string is NOT suppressed.
function isUpstreamEmptyGarbage(text: string): boolean {
  return text.trimStart().startsWith("(Empty response:");
}

export interface UseVoiceReplyOptions {
  currentAgent: string;
  messageSource: MessageSource;
  isStreaming: boolean;
  /** Composer-owned "preparing" indicator setter — set by this hook's effects. */
  setVoiceReplyActive: Dispatch<SetStateAction<boolean>>;
}

export interface UseVoiceReply {
  /** Cleanly END the voice turn so NOTHING more is voiced (Stop / `/stop`). */
  silenceVoiceTurn: () => void;
  /** Unlock TTS playback during a user gesture (mic tap / continuous toggle). */
  primeTtsAudio: () => void;
  /** "speaking" indicator — driven by the speaker's onStateChange. */
  ttsPlaying: boolean;
  /** The streaming TTS speaker (re-created per agent); the composer's continuous
   *  re-arm effect reads `speakerRef.current?.idle`. */
  speakerRef: React.RefObject<SpeakerQueue | null>;
}

export function useVoiceReply({
  currentAgent,
  messageSource,
  isStreaming,
  setVoiceReplyActive,
}: UseVoiceReplyOptions): UseVoiceReply {
  // Voice reply: speak the agent's answer aloud when the turn was sent by voice.
  // voiceTurnPending is the single source of truth (agent-state store field) for
  // "the turn about to start / that just started was voice-initiated" — set
  // either by a direct voice submit below (while idle) or by ChatThread's
  // pendingMessage drain (a queued voice message sent after streaming ends).
  // No local ref duplicates this: both the direct-submit and drained-queue
  // paths only ever arm the store flag.
  const voiceTurnPending = useChatStore((s) => s.agents[s.currentAgent]?.voiceTurnPending ?? false);
  // "speaking" indicator — driven by the speaker's onStateChange.
  const [ttsPlaying, setTtsPlaying] = useState(false);
  const ttsAudioRef = useRef<HTMLAudioElement | null>(null);
  const ttsUrlRef = useRef<string | null>(null);
  // Streaming per-sentence TTS pipeline (hooks/tts-speaker). The splitter turns
  // assistant text deltas into complete sentences; the speaker synthesises and
  // plays them in order on the single <audio> element. Both are (re)created per
  // agent in an effect below — never on every render.
  const speakerRef = useRef<SpeakerQueue | null>(null);
  const splitterRef = useRef<SentenceSplitter | null>(null);
  // Delta cursor: which assistant message + how much of its text has already been
  // fed to the splitter this turn.
  const feedRef = useRef<{ msgId: string | null; fedLen: number }>({ msgId: null, fedLen: 0 });
  // URL of the agent-audio part already handed to takeoverAudio this turn (guards
  // against re-takeover on every re-render once the audio part appears).
  const takenOverUrlRef = useRef<string | null>(null);
  // Settles + detaches the listeners of the currently-awaited deps.play promise
  // so a superseding play (takeover / next sentence) never leaks stale listeners
  // or hangs the pump on a promise that can no longer resolve.
  const playCleanupRef = useRef<(() => void) | null>(null);

  // ── Voice reply: speak the agent's answer (TTS playback) ──────────────────
  // One persistent <audio> element kept "unlocked" via primeTtsAudio() on a user
  // gesture, so the later (async) reply playback isn't blocked by autoplay policy.
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
    setTtsPlaying(false);
    setVoiceReplyActive(false);
  }, []);

  // Stop / `/stop`: cleanly END the voice turn so NOTHING more is voiced.
  // `stopStream`→`abortLocalOnly` sets connectionPhase="idle", indistinguishable
  // from a clean finish — so without this the streaming falling-edge effect would
  // still fire (voiceTurnPending true), run a final feed+flush and RE-enqueue a
  // trailing fragment after Stop. Clearing voiceTurnPending gates that effect out,
  // and resetting the feed cursor + splitter discards any accumulated partial
  // sentence so it can never be flushed later.
  const silenceVoiceTurn = useCallback(() => {
    speakerRef.current?.cancel();
    stopTts();
    useChatStore.getState().setVoiceTurnPending(false, currentAgent);
    feedRef.current = { msgId: null, fedLen: 0 };
    splitterRef.current = createSentenceSplitter();
    takenOverUrlRef.current = null;
  }, [stopTts, currentAgent]);

  const getTtsEl = useCallback(() => {
    if (!ttsAudioRef.current) ttsAudioRef.current = new Audio();
    return ttsAudioRef.current;
  }, []);

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

  // Single-output play sink for the speaker: routes every blob (per-sentence TTS
  // OR agent-audio takeover) through the one pre-unlocked <audio> element, so a
  // new play supersedes the old at the DOM level — that is how takeover stops
  // in-flight audio. Resolves on `ended`/`error`, on a rejected `play()`, or when
  // superseded (via playCleanupRef), so the module's pump never hangs.
  const playBlob = useCallback(
    (blob: Blob): Promise<void> => {
      const a = getTtsEl();
      playCleanupRef.current?.(); // settle any superseded play + detach its listeners
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

  // ── Streaming TTS speaker: (re)create per agent ───────────────────────────
  // deps.synth POSTs one sentence to /api/tts/synthesize?agent=… (409 →
  // tts_disabled → null, any other non-2xx / fetch error → null, never throws);
  // onStateChange/onDrain drive the "speaking"/"preparing" indicators and the
  // continuous re-arm gate.
  useEffect(() => {
    const agent = currentAgent;
    const splitter = createSentenceSplitter();
    const speaker = createSpeakerQueue({
      async synth(sentence, signal) {
        try {
          const resp = await fetch("/api/tts/synthesize?agent=" + encodeURIComponent(agent), {
            method: "POST",
            headers: {
              Authorization: `Bearer ${assertToken()}`,
              "Content-Type": "application/json",
            },
            body: JSON.stringify({ text: sentence }),
            signal,
          });
          if (resp.status === 409) {
            console.debug(`[tts] synthesis skipped — disabled for ${agent}`);
            return null;
          }
          if (!resp.ok) return null;
          return await resp.blob();
        } catch {
          return null; // abort / network error → skip this sentence, no throw
        }
      },
      play: (blob) => playBlob(blob),
      onStateChange: (s) => {
        setTtsPlaying(s === "speaking");
        // Any transition to idle also clears the "preparing" indicator — covers
        // agent-audio takeover, which by design fires no onDrain.
        if (s !== "speaking") setVoiceReplyActive(false);
      },
      onDrain: () => setVoiceReplyActive(false),
    });
    speakerRef.current = speaker;
    splitterRef.current = splitter;
    feedRef.current = { msgId: null, fedLen: 0 };
    takenOverUrlRef.current = null;
    return () => {
      speaker.cancel();
      stopTts();
      speakerRef.current = null;
      splitterRef.current = null;
    };
  }, [currentAgent, playBlob, stopTts]);

  // Feed the latest assistant message's NEW text into the splitter → speaker.
  // Only text parts (thinking / tool parts are excluded). A change of the last
  // assistant message id (a new tool-loop step) flushes the prior remainder and
  // resets the delta cursor.
  const feedFrom = useCallback((source: MessageSource) => {
    const speaker = speakerRef.current;
    const splitter = splitterRef.current;
    if (!speaker || !splitter) return;
    const last = findLastAssistant(source);
    if (!last) return;
    const text = assistantText(last);
    // Upstream "(Empty response: …)" garbage streams as text-delta but must never
    // be voiced. Checking the FULL accumulated prefix on every feed detects it
    // before any sentence could emit (the marker is 16 chars; the splitter holds
    // fragments < 20 chars) — so nothing is ever pushed for that turn.
    if (isUpstreamEmptyGarbage(text)) return;
    const feed = feedRef.current;
    if (feed.msgId !== last.id) {
      for (const s of splitter.flush()) speaker.enqueue(s);
      feed.msgId = last.id;
      feed.fedLen = 0;
    }
    if (text.length > feed.fedLen) {
      const delta = text.slice(feed.fedLen);
      feed.fedLen = text.length;
      for (const s of splitter.push(delta)) speaker.enqueue(s);
    }
  }, []);

  // Agent-audio takeover: an audio/* file part on the current voice turn's reply
  // (e.g. a synthesize_speech voice answer) supersedes per-sentence TTS — fetch
  // the blob and hand it to takeoverAudio (aborts synths, clears the queue, plays
  // just this). Uploads URLs are HMAC-signed (auth-exempt), so a plain fetch works.
  const handleTakeover = useCallback((source: MessageSource) => {
    if (!speakerRef.current) return;
    const last = findLastAssistant(source);
    if (!last) return;
    const audio = last.parts.find(
      (p): p is FilePart => p.type === "file" && p.mediaType.startsWith("audio"),
    );
    if (!audio || audio.url === takenOverUrlRef.current) return;
    takenOverUrlRef.current = audio.url;
    setVoiceReplyActive(true);
    const url = audio.url;
    const agentAtDispatch = currentAgent;
    void (async () => {
      try {
        const resp = await fetch(url);
        if (!resp.ok) return;
        const blob = await resp.blob();
        // The fetch above is non-abortable, so a blob can resolve AFTER the user
        // pressed Stop or switched agents. Only play it if this is still the
        // active voice turn for the same agent (and not superseded by a later
        // takeover). Otherwise drop it — no audio after Stop/agent-change.
        const st = useChatStore.getState();
        if (
          st.currentAgent !== agentAtDispatch ||
          !st.agents[agentAtDispatch]?.voiceTurnPending ||
          takenOverUrlRef.current !== url
        ) {
          return;
        }
        speakerRef.current?.takeoverAudio(blob);
      } catch {
        /* best-effort — a failed takeover leaves any queued sentences intact */
      }
    })();
  }, [currentAgent]);

  // New turn (rising edge of streaming): stop any leftover speech from the prior
  // reply and reset the per-turn feed/takeover cursors. Cancelling an idle
  // speaker preserves the just-armed "preparing" indicator (no state transition).
  // MUST run before the feed effect below so a new turn's freshly-fed sentences
  // are never cancelled by this reset (effects fire in definition order).
  const prevStreamingRisingRef = useRef(false);
  useEffect(() => {
    const was = prevStreamingRisingRef.current;
    prevStreamingRisingRef.current = isStreaming;
    if (!was && isStreaming) {
      speakerRef.current?.cancel();
      stopTts();
      splitterRef.current?.flush();
      feedRef.current = { msgId: null, fedLen: 0 };
      takenOverUrlRef.current = null;
      setVoiceReplyActive(voiceTurnPending);
    }
  }, [isStreaming, voiceTurnPending, stopTts]);

  // Drive the speaker from live messages — ONLY for a voice-initiated turn.
  // Non-voice turns never enqueue, so the speaker stays idle.
  useEffect(() => {
    if (!voiceTurnPending) return;
    if (isStreaming) feedFrom(messageSource);
    handleTakeover(messageSource);
  }, [messageSource, voiceTurnPending, isStreaming, feedFrom, handleTakeover]);

  // Turn finished (falling edge): flush the final remainder into the speaker,
  // clear the voice-turn flag, and — if nothing was queued to speak — drop the
  // indicator. voiceTurnPending covers both a direct voice submit and a drained
  // queued voice message.
  const prevStreamingRef = useRef(false);
  useEffect(() => {
    const was = prevStreamingRef.current;
    prevStreamingRef.current = isStreaming;
    if (was && !isStreaming && voiceTurnPending) {
      // Skip the final feed+flush entirely for an upstream-garbage turn so the
      // "(Empty response: …)" blob is never voiced (mirrors feedFrom's guard).
      const last = findLastAssistant(messageSource);
      const garbage = last ? isUpstreamEmptyGarbage(assistantText(last)) : false;
      if (!garbage) feedFrom(messageSource); // capture text that landed with `finish`
      const speaker = speakerRef.current;
      const splitter = splitterRef.current;
      if (speaker && splitter && !garbage) {
        for (const s of splitter.flush()) speaker.enqueue(s);
      }
      useChatStore.getState().setVoiceTurnPending(false, currentAgent);
      feedRef.current = { msgId: null, fedLen: 0 };
      if (speaker?.idle) setVoiceReplyActive(false);
    }
  }, [isStreaming, voiceTurnPending, messageSource, currentAgent, feedFrom]);

  return { silenceVoiceTurn, primeTtsAudio, ttsPlaying, speakerRef };
}
