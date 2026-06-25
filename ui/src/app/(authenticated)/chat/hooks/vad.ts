// ── hooks/vad.ts ─────────────────────────────────────────────────────────────
// Pure RMS-based Voice Activity Detection state machine (no DOM / Web Audio).
// Fed normalized RMS samples (0..1) with timestamps; emits "speech-confirmed"
// when speech starts and "silence-stop" when the speaker has gone quiet long
// enough to end the turn. Kept DOM-free so it is unit-testable in isolation;
// the recorder hook wires it to an AnalyserNode. Mirrors Hermes voice_mode's
// two-stage RMS approach (speech-confirm + silence-detect) — no webrtc/silero.

export interface VadConfig {
  /** Window at the start used to measure the ambient noise floor. */
  noiseCalibrationMs: number;
  /** Cumulative above-threshold time required to confirm speech started. */
  speechConfirmMs: number;
  /** Continuous below-threshold time that ends the turn. */
  silenceStopMs: number;
  /** Never emit silence-stop before this much total elapsed time. */
  minRecordingMs: number;
  /** threshold = max(noiseFloor * thresholdFloorMult, thresholdMin). */
  thresholdFloorMult: number;
  thresholdMin: number;
}

export const DEFAULT_VAD_CONFIG: VadConfig = {
  noiseCalibrationMs: 300,
  speechConfirmMs: 300,
  silenceStopMs: 2000,
  minRecordingMs: 500,
  thresholdFloorMult: 3,
  thresholdMin: 0.01,
};

export type VadEvent = "speech-confirmed" | "silence-stop";

export interface VadDetector {
  /** Feed one RMS sample (0..1) at time `nowMs`. Returns an event or null. */
  push(rms: number, nowMs: number): VadEvent | null;
  /** Reset to the initial (calibrating) state for reuse. */
  reset(): void;
  /** True once speech has been confirmed during the current run. */
  readonly speechDetected: boolean;
  /** The resolved threshold after calibration (0 while still calibrating). */
  readonly threshold: number;
}

type Phase = "calibrating" | "awaiting" | "speech";

export function createVadDetector(cfg: Partial<VadConfig> = {}): VadDetector {
  const c: VadConfig = { ...DEFAULT_VAD_CONFIG, ...cfg };

  let phase: Phase = "calibrating";
  let startMs: number | null = null;
  let lastMs: number | null = null;
  let floorSum = 0;
  let floorCount = 0;
  let threshold = c.thresholdMin;
  let speechAccumMs = 0;
  let silenceMs = 0;
  let speechDetected = false;

  function reset() {
    phase = "calibrating";
    startMs = null;
    lastMs = null;
    floorSum = 0;
    floorCount = 0;
    threshold = c.thresholdMin;
    speechAccumMs = 0;
    silenceMs = 0;
    speechDetected = false;
  }

  function push(rms: number, nowMs: number): VadEvent | null {
    if (startMs === null) startMs = nowMs;
    const dt = lastMs === null ? 0 : Math.max(0, nowMs - lastMs);
    lastMs = nowMs;
    const elapsed = nowMs - startMs;

    if (phase === "calibrating") {
      floorSum += rms;
      floorCount += 1;
      if (elapsed >= c.noiseCalibrationMs) {
        const floor = floorCount > 0 ? floorSum / floorCount : 0;
        threshold = Math.max(floor * c.thresholdFloorMult, c.thresholdMin);
        phase = "awaiting";
      }
      return null;
    }

    if (phase === "awaiting") {
      // Accumulate above-threshold time; micro-pauses between syllables (which
      // simply don't add) are tolerated until the cumulative target is hit.
      if (rms > threshold) {
        speechAccumMs += dt;
        if (speechAccumMs >= c.speechConfirmMs) {
          speechDetected = true;
          phase = "speech";
          silenceMs = 0;
          return "speech-confirmed";
        }
      }
      return null;
    }

    // phase === "speech": end the turn after continuous silence. Any sample at
    // or above threshold resets the counter, so pauses shorter than
    // silenceStopMs are inherently tolerated.
    if (rms > threshold) {
      silenceMs = 0;
    } else {
      silenceMs += dt;
      if (elapsed >= c.minRecordingMs && silenceMs >= c.silenceStopMs) {
        return "silence-stop";
      }
    }
    return null;
  }

  return {
    push,
    reset,
    get speechDetected() {
      return speechDetected;
    },
    get threshold() {
      return threshold;
    },
  };
}
