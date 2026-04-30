// ── hooks/use-voice-recorder.ts ──────────────────────────────────────────────
// Records audio via MediaRecorder, posts to /api/media/transcribe, returns transcript.
// Handles permission denial, auto-stop at 5 min, and cleanup on unmount.

import { useState, useRef, useCallback, useEffect } from "react";
import { assertToken } from "@/lib/api";

export type VoiceRecorderState = "idle" | "recording" | "transcribing" | "error";

export interface UseVoiceRecorder {
  state: VoiceRecorderState;
  /** Elapsed recording time in seconds. */
  elapsed: number;
  start: () => Promise<void>;
  /** Stop recording, transcribe, and return the transcript text. Returns "" on error. */
  stop: () => Promise<string>;
}

const MAX_RECORDING_SECS = 5 * 60; // 5 minutes

export function useVoiceRecorder(): UseVoiceRecorder {
  const [state, setState] = useState<VoiceRecorderState>("idle");
  const [elapsed, setElapsed] = useState(0);

  const recorderRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const streamRef = useRef<MediaStream | null>(null);
  const elapsedTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const autoStopTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mimeTypeRef = useRef<string>("audio/webm");

  const clearTimers = useCallback(() => {
    if (elapsedTimerRef.current !== null) {
      clearInterval(elapsedTimerRef.current);
      elapsedTimerRef.current = null;
    }
    if (autoStopTimerRef.current !== null) {
      clearTimeout(autoStopTimerRef.current);
      autoStopTimerRef.current = null;
    }
  }, []);

  const stopTracks = useCallback(() => {
    if (streamRef.current) {
      streamRef.current.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }
  }, []);

  // Cleanup on unmount.
  useEffect(() => {
    return () => {
      clearTimers();
      stopTracks();
      recorderRef.current?.stop();
    };
  }, [clearTimers, stopTracks]);

  const start = useCallback(async () => {
    if (state !== "idle") return;

    // Check for secure context (required by getUserMedia in most browsers).
    if (typeof window !== "undefined" && !window.isSecureContext && window.location.hostname !== "localhost") {
      const { toast } = await import("sonner");
      toast.error("Требуется HTTPS для доступа к микрофону");
      return;
    }

    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch {
      const { toast } = await import("sonner");
      toast.error("Нет доступа к микрофону");
      return;
    }

    streamRef.current = stream;
    chunksRef.current = [];

    // Pick the best available mime type.
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

    // Elapsed counter.
    elapsedTimerRef.current = setInterval(() => {
      setElapsed((s) => s + 1);
    }, 1000);

    // Auto-stop at 5 min.
    autoStopTimerRef.current = setTimeout(async () => {
      const { toast } = await import("sonner");
      toast.info("Запись автоматически остановлена (5 минут)");
      // stop() is safe to call multiple times.
    }, MAX_RECORDING_SECS * 1000);
  }, [state]);

  const stop = useCallback(async (): Promise<string> => {
    if (state !== "recording") return "";

    clearTimers();
    stopTracks();

    const recorder = recorderRef.current;
    if (!recorder || recorder.state === "inactive") {
      setState("idle");
      return "";
    }

    // Collect all audio chunks when recorder stops.
    const blob = await new Promise<Blob>((resolve) => {
      recorder.onstop = () => {
        const b = new Blob(chunksRef.current, { type: mimeTypeRef.current });
        resolve(b);
      };
      recorder.stop();
    });

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
      toast.error(`Не удалось распознать речь: ${err instanceof Error ? err.message : "ошибка"}`);
      setState("idle");
      setElapsed(0);
      return "";
    }
  }, [state, clearTimers, stopTracks]);

  return { state, elapsed, start, stop };
}
