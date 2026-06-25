// ── hooks/use-voice-recorder.ts ──────────────────────────────────────────────
// Records audio via MediaRecorder, posts to /api/media/transcribe, returns transcript.
// Handles permission denial, auto-stop at 5 min, and cleanup on unmount.
//
// Optional client-side VAD (Voice Activity Detection): when `vad` is enabled, an
// AnalyserNode feeds normalized RMS into the pure `vad.ts` state machine to
// auto-stop on silence, expose a live `level`, and skip transcribing empty
// (speechless) recordings. Manual tap-to-stop always works regardless.

import { useState, useRef, useCallback, useEffect } from "react";
import { assertToken } from "@/lib/api";
import { useTranslation } from "@/hooks/use-translation";
import { createVadDetector, type VadDetector } from "./vad";

export type VoiceRecorderState = "idle" | "recording" | "transcribing" | "error";

export interface UseVoiceRecorderOptions {
  /** Enable client-side VAD: auto-stop on silence, level metering, skip-empty. */
  vad?: boolean;
  /** Called when a VAD-triggered auto-stop produced a result. `""` = empty/skipped. */
  onAutoResult?: (text: string) => void;
}

export interface UseVoiceRecorder {
  state: VoiceRecorderState;
  /** Elapsed recording time in seconds. */
  elapsed: number;
  /** Smoothed input level 0..1 while recording; 0 otherwise. */
  level: number;
  start: () => Promise<void>;
  /** Stop recording, transcribe, and return the transcript text. Returns "" on error/empty. */
  stop: () => Promise<string>;
}

const MAX_RECORDING_SECS = 5 * 60; // 5 minutes
const SAMPLE_INTERVAL_MS = 50;

export function useVoiceRecorder(opts: UseVoiceRecorderOptions = {}): UseVoiceRecorder {
  const { vad = false } = opts;
  const { t } = useTranslation();
  const [state, setState] = useState<VoiceRecorderState>("idle");
  const [elapsed, setElapsed] = useState(0);
  const [level, setLevel] = useState(0);

  const recorderRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const streamRef = useRef<MediaStream | null>(null);
  const elapsedTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const autoStopTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mimeTypeRef = useRef<string>("audio/webm");

  // ── VAD plumbing ──────────────────────────────────────────────────────────
  const audioCtxRef = useRef<AudioContext | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const sampleTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const vadRef = useRef<VadDetector | null>(null);
  const finishingRef = useRef(false);
  // Latest onAutoResult without re-binding callbacks.
  const onAutoResultRef = useRef(opts.onAutoResult);
  useEffect(() => {
    onAutoResultRef.current = opts.onAutoResult;
  });

  const clearTimers = useCallback(() => {
    if (elapsedTimerRef.current !== null) {
      clearInterval(elapsedTimerRef.current);
      elapsedTimerRef.current = null;
    }
    if (autoStopTimerRef.current !== null) {
      clearTimeout(autoStopTimerRef.current);
      autoStopTimerRef.current = null;
    }
    if (sampleTimerRef.current !== null) {
      clearInterval(sampleTimerRef.current);
      sampleTimerRef.current = null;
    }
  }, []);

  const teardownAudio = useCallback(() => {
    analyserRef.current = null;
    vadRef.current = null;
    if (audioCtxRef.current) {
      void audioCtxRef.current.close().catch(() => {});
      audioCtxRef.current = null;
    }
  }, []);

  const stopTracks = useCallback(() => {
    if (streamRef.current) {
      streamRef.current.getTracks().forEach((tr) => tr.stop());
      streamRef.current = null;
    }
  }, []);

  // Cleanup on unmount.
  useEffect(() => {
    return () => {
      clearTimers();
      teardownAudio();
      stopTracks();
      recorderRef.current?.stop();
    };
  }, [clearTimers, teardownAudio, stopTracks]);

  /** Stop recording, transcribe the captured audio, and return the transcript.
   *  Idempotent (guards against the VAD auto-stop racing a manual stop). */
  const finalize = useCallback(async (): Promise<string> => {
    if (finishingRef.current) return "";
    finishingRef.current = true;

    const skipEmpty = vad && vadRef.current ? !vadRef.current.speechDetected : false;

    clearTimers();
    teardownAudio();
    stopTracks();
    setLevel(0);

    const recorder = recorderRef.current;
    if (!recorder || recorder.state === "inactive") {
      setState("idle");
      return "";
    }

    const blob = await new Promise<Blob>((resolve) => {
      recorder.onstop = () => resolve(new Blob(chunksRef.current, { type: mimeTypeRef.current }));
      recorder.stop();
    });

    // VAD detected only silence — don't waste an STT call.
    if (skipEmpty) {
      setState("idle");
      setElapsed(0);
      return "";
    }

    setState("transcribing");
    try {
      const ext = mimeTypeRef.current.includes("mp4") ? "mp4" : "webm";
      const formData = new FormData();
      formData.append("file", blob, `recording.${ext}`);

      const resp = await fetch("/api/media/transcribe", {
        method: "POST",
        headers: { Authorization: `Bearer ${assertToken()}` },
        body: formData,
      });
      if (!resp.ok) {
        const err = await resp.text().catch(() => resp.statusText);
        throw new Error(err);
      }
      const data = await resp.json();
      const text: string = data.text ?? "";
      setState("idle");
      setElapsed(0);
      return text;
    } catch (err) {
      const { toast } = await import("sonner");
      toast.error(t("chat.voice_recognize_error", { error: err instanceof Error ? err.message : "unknown" }));
      setState("idle");
      setElapsed(0);
      return "";
    }
  }, [vad, clearTimers, teardownAudio, stopTracks, t]);

  const start = useCallback(async () => {
    if (state !== "idle") return;

    // Secure context required by getUserMedia in most browsers.
    if (typeof window !== "undefined" && !window.isSecureContext && window.location.hostname !== "localhost") {
      const { toast } = await import("sonner");
      toast.error(t("chat.voice_requires_https"));
      return;
    }

    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch {
      const { toast } = await import("sonner");
      toast.error(t("chat.voice_no_permission"));
      return;
    }

    streamRef.current = stream;
    chunksRef.current = [];
    finishingRef.current = false;

    const mime = MediaRecorder.isTypeSupported("audio/webm") ? "audio/webm" : "audio/mp4";
    mimeTypeRef.current = mime;

    const recorder = new MediaRecorder(stream, { mimeType: mime });
    recorderRef.current = recorder;
    recorder.ondataavailable = (e) => {
      if (e.data.size > 0) chunksRef.current.push(e.data);
    };
    recorder.start(250); // collect chunks every 250ms
    setState("recording");
    setElapsed(0);
    setLevel(0);

    // ── VAD: analyser + sampling loop ───────────────────────────────────────
    if (vad) {
      try {
        const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
        const ctx = new Ctx();
        audioCtxRef.current = ctx;
        const source = ctx.createMediaStreamSource(stream);
        const analyser = ctx.createAnalyser();
        analyser.fftSize = 2048;
        source.connect(analyser);
        analyserRef.current = analyser;
        vadRef.current = createVadDetector();

        const buf = new Float32Array(analyser.fftSize);
        sampleTimerRef.current = setInterval(() => {
          const an = analyserRef.current;
          const det = vadRef.current;
          if (!an) return;
          an.getFloatTimeDomainData(buf);
          let sum = 0;
          for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i];
          const rms = Math.sqrt(sum / buf.length);
          setLevel((prev) => prev * 0.6 + rms * 0.4);
          if (det) {
            const evt = det.push(rms, performance.now());
            if (evt === "silence-stop") {
              void finalize().then((text) => onAutoResultRef.current?.(text));
            }
          }
        }, SAMPLE_INTERVAL_MS);
      } catch {
        // Web Audio unavailable — fall back to manual stop only.
        teardownAudio();
      }
    }

    // Elapsed counter.
    elapsedTimerRef.current = setInterval(() => {
      setElapsed((s) => s + 1);
    }, 1000);

    // Hard cap at 5 min — actually finalize (previous code only toasted).
    autoStopTimerRef.current = setTimeout(() => {
      void (async () => {
        const { toast } = await import("sonner");
        toast.info(t("chat.voice_auto_stopped"));
        const text = await finalize();
        if (vad) onAutoResultRef.current?.(text);
      })();
    }, MAX_RECORDING_SECS * 1000);
  }, [state, vad, t, finalize, teardownAudio]);

  const stop = useCallback(async (): Promise<string> => {
    if (state !== "recording") return "";
    return finalize();
  }, [state, finalize]);

  return { state, elapsed, level, start, stop };
}
