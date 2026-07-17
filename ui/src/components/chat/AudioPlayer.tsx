"use client";

import { useState, useRef, useCallback, useEffect, useMemo } from "react";
import { Play, Pause } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";

// Single visual language for both shape AND progress: a bar waveform built
// from the actual audio buffer (RMS per bucket), where bars to the left of
// the playhead are coloured primary and the rest fade into muted-foreground.
// Click anywhere on the bars to seek. Mirrors the Telegram/Threads voice
// message UX — one canvas-free, accessible, mobile-friendly element.

const BAR_COUNT = 48;
const MIN_BAR_HEIGHT = 0.12; // floor so quiet samples still show as a tick

function formatTime(secs: number): string {
  if (!isFinite(secs) || secs < 0) return "0:00";
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

// Pleasant placeholder shape used until real bars are decoded — three
// detuned sines so the player never looks empty during the network round-trip.
function makePlaceholderBars(): number[] {
  return Array.from({ length: BAR_COUNT }, (_, i) => {
    const t = i / (BAR_COUNT - 1);
    const a =
      Math.sin(t * Math.PI * 4) * 0.30 +
      Math.sin(t * Math.PI * 9 + 0.7) * 0.18 +
      Math.sin(t * Math.PI * 17 + 1.3) * 0.10;
    return Math.max(MIN_BAR_HEIGHT, Math.min(1, 0.45 + a));
  });
}

// Decode the audio file once and reduce it to BAR_COUNT amplitudes (RMS per
// bucket, normalised to the peak so quiet recordings still fill the row).
// Aborts cleanly on unmount.
async function decodeBars(src: string, signal: AbortSignal): Promise<number[] | null> {
  try {
    const resp = await fetch(src, { signal });
    if (!resp.ok) return null;
    const buf = await resp.arrayBuffer();
    if (signal.aborted) return null;
    // Web Audio: a fresh context per decode is fine — close it immediately.
    const Ctor: typeof AudioContext =
      window.AudioContext || (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
    if (!Ctor) return null;
    const ctx = new Ctor();
    const audioBuf = await ctx.decodeAudioData(buf.slice(0));
    await ctx.close();
    if (signal.aborted) return null;

    const channelData = audioBuf.getChannelData(0);
    const samplesPerBar = Math.max(1, Math.floor(channelData.length / BAR_COUNT));
    const bars: number[] = new Array(BAR_COUNT);
    let peak = 0;
    for (let b = 0; b < BAR_COUNT; b++) {
      let sumSq = 0;
      const start = b * samplesPerBar;
      const end = Math.min(channelData.length, start + samplesPerBar);
      for (let i = start; i < end; i++) {
        const v = channelData[i];
        sumSq += v * v;
      }
      const rms = Math.sqrt(sumSq / Math.max(1, end - start));
      bars[b] = rms;
      if (rms > peak) peak = rms;
    }
    if (peak === 0) return null;
    return bars.map(v => Math.max(MIN_BAR_HEIGHT, v / peak));
  } catch {
    return null;
  }
}

export function AudioPlayer({ src }: { src: string }) {
  const { t } = useTranslation();
  const audioRef = useRef<HTMLAudioElement>(null);
  const barsRef = useRef<HTMLDivElement>(null);

  const [bars, setBars] = useState<number[]>(makePlaceholderBars);
  const [decoded, setDecoded] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  // Tracked in state (not read from audioRef during render) so React re-renders
  // when the streaming buffer extends. See react-hooks/refs lint rule.
  const [seekableEnd, setSeekableEnd] = useState(0);
  const [error, setError] = useState(false);

  // ── Decode bars once per src ──────────────────────────────────────────────
  useEffect(() => {
    setDecoded(false);
    setBars(makePlaceholderBars());
    const ctrl = new AbortController();
    decodeBars(src, ctrl.signal).then(b => {
      if (b) {
        setBars(b);
        setDecoded(true);
      }
    });
    return () => ctrl.abort();
  }, [src]);

  // ── Audio handlers ────────────────────────────────────────────────────────
  const handleDurationUpdate = useCallback(() => {
    const a = audioRef.current;
    if (!a) return;
    const d = a.duration;
    if (d && isFinite(d) && d > 0) setDuration(d);
    if (a.seekable?.length) setSeekableEnd(a.seekable.end(0));
  }, []);

  const togglePlay = useCallback(() => {
    const a = audioRef.current;
    if (!a) return;
    if (playing) a.pause();
    else a.play().catch(() => setError(true));
  }, [playing]);

  // Streaming WAVs (TTS) sometimes report Infinity for duration — fall back
  // to seekable.end (tracked in state, updated by audio events).
  const seekMax = duration > 0 ? duration : seekableEnd;
  const canSeek = seekMax > 0 && !error;
  const progress = seekMax > 0 ? Math.min(1, currentTime / seekMax) : 0;

  // Click / drag anywhere on the bars to seek. Pointer events cover mouse +
  // touch + stylus uniformly; capture lets us track drag past the element.
  const seekFromPointer = useCallback(
    (clientX: number) => {
      const el = barsRef.current;
      const a = audioRef.current;
      if (!el || !a || !canSeek) return;
      const rect = el.getBoundingClientRect();
      const ratio = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
      const t = ratio * seekMax;
      try {
        a.currentTime = t;
      } catch {
        /* unseekable mid-stream — ignore */
      }
      setCurrentTime(t);
    },
    [canSeek, seekMax],
  );

  const draggingRef = useRef(false);
  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!canSeek) return;
      draggingRef.current = true;
      e.currentTarget.setPointerCapture(e.pointerId);
      seekFromPointer(e.clientX);
    },
    [canSeek, seekFromPointer],
  );
  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!draggingRef.current) return;
      seekFromPointer(e.clientX);
    },
    [seekFromPointer],
  );
  const onPointerUp = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      /* pointer already released */
    }
  }, []);

  // Keyboard: ←/→ seek by 5s, space toggles play. Hooked on the bars region
  // because the play button has its own native space/enter handling.
  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      const a = audioRef.current;
      if (!a) return;
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        try {
          a.currentTime = Math.max(0, (a.currentTime || 0) - 5);
        } catch {
          /* no-op */
        }
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        try {
          a.currentTime = Math.min(seekMax || a.currentTime, (a.currentTime || 0) + 5);
        } catch {
          /* no-op */
        }
      } else if (e.key === " " || e.key === "Spacebar") {
        e.preventDefault();
        togglePlay();
      }
    },
    [seekMax, togglePlay],
  );

  // Time label: always "current / total" once we know the duration so the
  // user sees both at a glance — no mode-switch between rest and playback.
  // Streaming audio without a duration header still falls back to current.
  const timeLabel = useMemo(() => {
    if (duration > 0) return `${formatTime(currentTime)} / ${formatTime(duration)}`;
    return formatTime(currentTime);
  }, [currentTime, duration]);

  return (
    <div
      className="audio-player w-full max-w-md rounded-2xl border border-border bg-card"
      style={{ padding: "12px 14px" }}
      data-decoded={decoded || undefined}
      data-playing={playing || undefined}
    >
      {/* <audio> without controls renders nothing */}
      <audio
        ref={audioRef}
        src={src}
        preload="metadata"
        onLoadedMetadata={handleDurationUpdate}
        onDurationChange={handleDurationUpdate}
        onCanPlay={handleDurationUpdate}
        onTimeUpdate={() => setCurrentTime(audioRef.current?.currentTime ?? 0)}
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => {
          setPlaying(false);
          setCurrentTime(0);
          handleDurationUpdate();
        }}
        onError={() => {
          setPlaying(false);
          setError(true);
        }}
      />

      <div className="flex items-center gap-3">
        {/* Play / Pause */}
        <button
          onClick={togglePlay}
          disabled={error}
          aria-label={playing ? t("chat.audio_pause") : t("chat.audio_play")}
          className="relative flex-shrink-0 w-9 h-9 rounded-xl flex items-center justify-center focus:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 transition-transform duration-100 active:scale-95"
          style={{ background: "var(--primary)" }}
        >
          {playing && (
            <span
              className="absolute inset-0 rounded-xl pointer-events-none"
              style={{
                background: "var(--primary)",
                opacity: 0.18,
                animation: "audio-player-pulse 1.6s ease-in-out infinite",
              }}
            />
          )}
          {playing ? (
            <Pause className="h-3.5 w-3.5 relative" style={{ color: "var(--primary-foreground)" }} />
          ) : (
            <Play className="h-3.5 w-3.5 ml-0.5 relative" style={{ color: "var(--primary-foreground)" }} />
          )}
        </button>

        {/* Bar waveform — fills with primary up to playhead, the rest fades.
            Click + drag anywhere to seek. Keyboard: ←/→ seek 5s, space play. */}
        <div
          ref={barsRef}
          role="slider"
          tabIndex={canSeek ? 0 : -1}
          aria-label={t("chat.audio_seek")}
          aria-valuemin={0}
          aria-valuemax={seekMax > 0 ? Math.round(seekMax) : 0}
          aria-valuenow={Math.round(currentTime)}
          aria-valuetext={`${formatTime(currentTime)} / ${formatTime(seekMax)}`}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onKeyDown={onKeyDown}
          className="relative flex-1 min-w-0 h-9 cursor-pointer select-none touch-none focus:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-md"
          style={{ touchAction: "none" }}
        >
          <div className="absolute inset-0 flex items-center gap-0.5 px-px">
            {bars.map((amp, i) => {
              // One bar = one slot of the row. Filled iff its slot is to the
              // LEFT of the playhead (use centre of the slot for fairness).
              const slotCentre = (i + 0.5) / BAR_COUNT;
              const filled = slotCentre <= progress;
              return (
                <div
                  key={i}
                  className="flex-1 rounded-xs transition-[background-color,opacity] duration-75 ease-out"
                  style={{
                    height: `${Math.max(MIN_BAR_HEIGHT, amp) * 100}%`,
                    minHeight: 2,
                    background: filled ? "var(--primary)" : "var(--muted-foreground)",
                    opacity: filled ? 0.95 : 0.32,
                  }}
                />
              );
            })}
          </div>
        </div>

        {/* Time */}
        <span
          className="flex-shrink-0 text-2xs tabular-nums leading-none"
          style={{
            color: "var(--muted-foreground)",
            fontFamily: "var(--font-mono, monospace)",
            minWidth: duration > 0 ? "9ch" : "4ch",
            textAlign: "right",
          }}
        >
          {timeLabel}
        </span>
      </div>

      {error && (
        <p className="mt-1 text-2xs" style={{ color: "var(--destructive)" }}>
          {t("chat.audio_play_error")}
        </p>
      )}

      <style>{`
        @keyframes audio-player-pulse {
          0%, 100% { transform: scale(1);   opacity: 0.18; }
          50%      { transform: scale(1.08); opacity: 0.04; }
        }
        @media (prefers-reduced-motion: reduce) {
          .audio-player [style*="audio-player-pulse"] { animation: none !important; }
        }
      `}</style>
    </div>
  );
}
