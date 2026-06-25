import { describe, it, expect } from "vitest";
import { createVadDetector } from "../vad";

/** Feed a sequence of RMS samples at a fixed interval; collect emitted events. */
function feed(
  d: ReturnType<typeof createVadDetector>,
  samples: number[],
  intervalMs = 50,
  startAt = 0,
) {
  const events: string[] = [];
  let t = startAt;
  for (const rms of samples) {
    const e = d.push(rms, t);
    if (e) events.push(e);
    t += intervalMs;
  }
  return { events, endMs: t - intervalMs };
}

const LOW = 0.005; // below threshold (noise floor)
const HIGH = 0.1; // clearly speech
const rep = (v: number, n: number) => Array.from({ length: n }, () => v);

describe("vad detector", () => {
  it("emits speech-confirmed on sustained speech after calibration", () => {
    const d = createVadDetector();
    // ~400ms calibration (low) then sustained high
    const { events } = feed(d, [...rep(LOW, 8), ...rep(HIGH, 12)]);
    expect(events).toContain("speech-confirmed");
    expect(d.speechDetected).toBe(true);
  });

  it("emits silence-stop after speech followed by >2s silence", () => {
    const d = createVadDetector();
    // calibrate, confirm speech, then long silence (>2000ms = 45 samples)
    const { events } = feed(d, [...rep(LOW, 8), ...rep(HIGH, 12), ...rep(LOW, 45)]);
    expect(events).toContain("speech-confirmed");
    expect(events).toContain("silence-stop");
    // speech-confirmed must come before silence-stop
    expect(events.indexOf("speech-confirmed")).toBeLessThan(events.indexOf("silence-stop"));
  });

  it("tolerates a short dip during speech without stopping", () => {
    const d = createVadDetector();
    // confirm speech, brief 200ms dip (4 low), resume speech — no silence-stop
    const { events } = feed(d, [
      ...rep(LOW, 8),
      ...rep(HIGH, 12),
      ...rep(LOW, 4), // 200ms dip << 2000ms
      ...rep(HIGH, 12),
    ]);
    expect(events).toContain("speech-confirmed");
    expect(events).not.toContain("silence-stop");
  });

  it("never confirms speech on pure silence (skip-empty)", () => {
    const d = createVadDetector();
    const { events } = feed(d, rep(LOW, 60)); // 3s of silence
    expect(events).not.toContain("speech-confirmed");
    expect(events).not.toContain("silence-stop");
    expect(d.speechDetected).toBe(false);
  });

  it("reset() clears state so the detector can be reused", () => {
    const d = createVadDetector();
    feed(d, [...rep(LOW, 8), ...rep(HIGH, 12)]);
    expect(d.speechDetected).toBe(true);
    d.reset();
    expect(d.speechDetected).toBe(false);
    // After reset, pure silence stays silent
    const { events } = feed(d, rep(LOW, 20));
    expect(events).not.toContain("speech-confirmed");
  });
});
