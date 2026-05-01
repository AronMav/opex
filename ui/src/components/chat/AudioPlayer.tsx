"use client";

import { useState, useRef, useCallback, useEffect } from "react";
import { Play, Pause, Volume2, VolumeX } from "lucide-react";

function formatTime(secs: number): string {
  if (!isFinite(secs) || secs < 0) return "0:00";
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

// Static waveform shape for paused state — three overlapping sine waves
const STATIC_WAVE: { x: number; y: number }[] = Array.from({ length: 120 }, (_, i) => {
  const t = i / 119;
  const amp =
    Math.sin(t * Math.PI * 7) * 0.22 +
    Math.sin(t * Math.PI * 13 + 1.5) * 0.12 +
    Math.sin(t * Math.PI * 23 + 0.8) * 0.07;
  return { x: t, y: 0.5 + amp };
});

function getCSSColor(varName: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  return getComputedStyle(document.documentElement).getPropertyValue(varName).trim() || fallback;
}

function strokePath(
  ctx: CanvasRenderingContext2D,
  pts: { x: number; y: number }[],
  W: number,
  H: number,
  color: string,
  opacity: number,
  glow: boolean,
) {
  ctx.save();
  ctx.globalAlpha = opacity;
  ctx.strokeStyle = color;
  ctx.lineWidth = 1.5;
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  if (glow) { ctx.shadowBlur = 10; ctx.shadowColor = color; }
  ctx.beginPath();
  pts.forEach((p, i) => {
    const x = p.x * W, y = p.y * H;
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.stroke();
  ctx.restore();
}

export function AudioPlayer({ src }: { src: string }) {
  const audioRef  = useRef<HTMLAudioElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const audioCtxRef  = useRef<AudioContext | null>(null);
  const analyserRef  = useRef<AnalyserNode | null>(null);
  const dataArrayRef = useRef<Uint8Array<ArrayBuffer> | null>(null);
  const rafRef       = useRef<number>(0);
  // Refs for RAF loop — avoids stale closures without re-creating the loop
  const currentTimeRef = useRef(0);
  const durationRef    = useRef(0);

  const [playing,     setPlaying]     = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration,    setDuration]    = useState(0);
  const [muted,       setMuted]       = useState(false);
  const [error,       setError]       = useState(false);

  useEffect(() => { currentTimeRef.current = currentTime; }, [currentTime]);
  useEffect(() => { durationRef.current    = duration;    }, [duration]);

  // ── Canvas ────────────────────────────────────────────────────────────────

  const setupCanvas = useCallback(() => {
    const c = canvasRef.current;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    const r   = c.getBoundingClientRect();
    if (r.width === 0) return;
    c.width  = Math.round(r.width  * dpr);
    c.height = Math.round(r.height * dpr);
  }, []);

  // Draw the pre-baked static shape — used when paused
  const drawStatic = useCallback(() => {
    const c = canvasRef.current;
    if (!c || c.width === 0) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const W = c.width / dpr, H = c.height / dpr;
    ctx.clearRect(0, 0, c.width, c.height);
    ctx.save(); ctx.scale(dpr, dpr);
    strokePath(ctx, STATIC_WAVE, W, H, getCSSColor("--muted-foreground", "#8b96a8"), 0.3, false);
    ctx.restore();
  }, []);

  // Live oscilloscope RAF loop — single primary-colour line, no progress split
  const startLiveLoop = useCallback(() => {
    const loop = () => {
      const c        = canvasRef.current;
      const analyser = analyserRef.current;
      const data     = dataArrayRef.current;
      if (!c || !analyser || !data || c.width === 0) {
        rafRef.current = requestAnimationFrame(loop);
        return;
      }
      analyser.getByteTimeDomainData(data);
      const ctx = c.getContext("2d");
      if (!ctx) { rafRef.current = requestAnimationFrame(loop); return; }

      const dpr = window.devicePixelRatio || 1;
      const W = c.width / dpr, H = c.height / dpr;
      const N = data.length;

      // Amplify: TTS deviation ≈ ±10–20 out of 128, ×2.5 fills the canvas nicely
      const GAIN = 2.5;
      const pts: { x: number; y: number }[] = Array.from({ length: N }, (_, i) => ({
        x: i / (N - 1),
        y: Math.max(0.04, Math.min(0.96, 0.5 + ((data[i] - 128) / 128) * GAIN)),
      }));

      ctx.clearRect(0, 0, c.width, c.height);
      ctx.save(); ctx.scale(dpr, dpr);
      strokePath(ctx, pts, W, H, getCSSColor("--primary", "#6b9eff"), 0.9, true);
      ctx.restore();

      rafRef.current = requestAnimationFrame(loop);
    };
    rafRef.current = requestAnimationFrame(loop);
  }, []);

  // ── AudioContext init (lazy, requires user gesture) ────────────────────────

  const initAudioContext = useCallback(() => {
    if (analyserRef.current || !audioRef.current) return;
    try {
      const actx    = new AudioContext();
      const analyser = actx.createAnalyser();
      analyser.fftSize              = 256;
      analyser.smoothingTimeConstant = 0.55;
      const source = actx.createMediaElementSource(audioRef.current);
      source.connect(analyser);
      analyser.connect(actx.destination);
      audioCtxRef.current  = actx;
      analyserRef.current  = analyser;
      dataArrayRef.current = new Uint8Array(analyser.frequencyBinCount);
    } catch { /* CORS or policy — visualisation falls back to static */ }
  }, []);

  // ── Effects ───────────────────────────────────────────────────────────────

  useEffect(() => {
    // Wait one frame so the canvas has layout dimensions
    const id = requestAnimationFrame(() => { setupCanvas(); drawStatic(); });
    const ro = new ResizeObserver(() => { setupCanvas(); if (!analyserRef.current) drawStatic(); });
    if (canvasRef.current) ro.observe(canvasRef.current);
    return () => {
      cancelAnimationFrame(id);
      cancelAnimationFrame(rafRef.current);
      ro.disconnect();
      audioCtxRef.current?.close();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (playing) {
      initAudioContext();
      cancelAnimationFrame(rafRef.current);
      if (analyserRef.current) startLiveLoop();
    } else {
      cancelAnimationFrame(rafRef.current);
      drawStatic();
    }
  }, [playing, initAudioContext, startLiveLoop, drawStatic]);

  // ── Audio handlers ────────────────────────────────────────────────────────

  const handleDurationUpdate = useCallback(() => {
    const d = audioRef.current?.duration;
    if (d && isFinite(d) && d > 0) setDuration(d);
  }, []);

  const togglePlay = useCallback(() => {
    const a = audioRef.current;
    if (!a) return;
    if (playing) a.pause(); else a.play().catch(() => setError(true));
  }, [playing]);

  // Scrubber seeks via the audio element's seekable range (works even when
  // duration is Infinity for streaming audio — uses seekable.end(0) as ceiling)
  const handleSeek = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const a = audioRef.current;
    if (!a) return;
    const t = Number(e.target.value);
    try { a.currentTime = t; } catch { return; }
    setCurrentTime(t);
  }, []);

  const toggleMute = useCallback(() => {
    const a = audioRef.current;
    if (!a) return;
    a.muted = !muted;
    setMuted(m => !m);
  }, [muted]);

  // Use seekable end as max if duration is unknown (streaming WAV without header length)
  const seekMax = duration > 0
    ? duration
    : (audioRef.current?.seekable?.length
        ? audioRef.current.seekable.end(0)
        : 0);
  const canSeek  = seekMax > 0 && !error;
  const progress = seekMax > 0 ? Math.min(1, currentTime / seekMax) : 0;

  return (
    <div
      className={`audio-player w-full max-w-md rounded-2xl border border-border bg-card${playing ? " audio-player-playing" : ""}`}
      style={{ padding: "12px 14px 10px" }}
    >
      {/* <audio> without controls renders nothing — no need to hide it */}
      <audio
        ref={audioRef}
        src={src}
        preload="auto"
        onLoadedMetadata={handleDurationUpdate}
        onDurationChange={handleDurationUpdate}
        onCanPlay={handleDurationUpdate}
        onTimeUpdate={() => setCurrentTime(audioRef.current?.currentTime ?? 0)}
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => { setPlaying(false); handleDurationUpdate(); }}
        onError={() => { setPlaying(false); setError(true); }}
      />

      {/* ── Controls row ── */}
      <div className="flex items-center gap-3">
        {/* Play / Pause */}
        <button
          onClick={togglePlay}
          disabled={error}
          aria-label={playing ? "Пауза" : "Играть"}
          className="relative flex-shrink-0 w-9 h-9 rounded-xl flex items-center justify-center focus:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 transition-transform duration-100 active:scale-95"
          style={{ background: "var(--primary)" }}
        >
          {playing && (
            <span
              className="absolute inset-0 rounded-xl opacity-20 pointer-events-none"
              style={{ background: "var(--primary)", animation: "thin-pulse 1.8s ease-in-out infinite" }}
            />
          )}
          {playing
            ? <Pause className="h-3.5 w-3.5" style={{ color: "var(--primary-foreground)" }} />
            : <Play  className="h-3.5 w-3.5 ml-0.5" style={{ color: "var(--primary-foreground)" }} />
          }
        </button>

        {/* Oscilloscope — single colour line, no progress split */}
        <canvas ref={canvasRef} className="flex-1 h-8" aria-hidden="true" />

        {/* Current time only + mute */}
        <div className="flex-shrink-0 flex items-center gap-2">
          <span
            className="text-[11px] tabular-nums leading-none"
            style={{ color: "var(--muted-foreground)", fontFamily: "var(--font-mono, monospace)", minWidth: "2.8ch" }}
          >
            {formatTime(currentTime)}
          </span>
          <button
            onClick={toggleMute}
            className="focus:outline-none focus-visible:ring-1 focus-visible:ring-ring rounded transition-opacity hover:opacity-100"
            style={{ color: "var(--muted-foreground)", opacity: 0.4 }}
            aria-label={muted ? "Включить звук" : "Выключить звук"}
          >
            {muted ? <VolumeX className="h-3.5 w-3.5" /> : <Volume2 className="h-3.5 w-3.5" />}
          </button>
        </div>
      </div>

      {/* ── Scrubber ── */}
      <div className="relative mt-2.5 py-2 group/scrubber">
        {/* Track */}
        <div className="relative h-[2px] rounded-full" style={{ background: "var(--border)" }}>
          {/* Fill */}
          <div
            className="absolute inset-y-0 left-0 rounded-full transition-[width] duration-100"
            style={{ width: `${progress * 100}%`, background: "var(--primary)" }}
          />
          {/* Thumb — appears on hover */}
          <div
            className="absolute top-1/2 w-2.5 h-2.5 rounded-full pointer-events-none opacity-0 group-hover/scrubber:opacity-100 transition-opacity duration-150"
            style={{
              left: `${progress * 100}%`,
              transform: "translate(-50%, -50%)",
              background: "var(--primary)",
              boxShadow: "0 0 0 2px var(--card)",
            }}
          />
        </div>
        {/* Native range input — invisible, handles all drag interaction */}
        <input
          type="range"
          min={0}
          max={seekMax || 0}
          step={0.05}
          value={canSeek ? currentTime : 0}
          onChange={handleSeek}
          disabled={!canSeek}
          className="absolute inset-0 w-full h-full opacity-0 cursor-pointer disabled:cursor-default"
          style={{ margin: 0, padding: 0 }}
          aria-label="Перемотка"
        />
      </div>

      {error && (
        <p className="mt-1 text-[11px]" style={{ color: "var(--destructive)" }}>
          Не удалось воспроизвести аудио
        </p>
      )}
    </div>
  );
}
