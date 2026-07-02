import { test, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k }),
}));
vi.mock("@/lib/api", () => ({ assertToken: () => "tok" }));

import { useVoiceRecorder } from "../use-voice-recorder";

// Minimal MediaRecorder stub — counts how many recorders get constructed.
class FakeMediaRecorder {
  static instances = 0;
  static isTypeSupported() {
    return true;
  }
  state = "recording";
  ondataavailable: ((e: { data: Blob }) => void) | null = null;
  onstop: (() => void) | null = null;
  constructor() {
    FakeMediaRecorder.instances++;
  }
  start() {
    // Emit one chunk > 1KB so finalize() proceeds past the "clipped capture" skip
    // and actually reaches the transcribe fetch.
    this.ondataavailable?.({ data: new Blob(["x".repeat(2000)]) });
  }
  stop() {
    this.state = "inactive";
    this.onstop?.();
  }
}

let getUserMedia: ReturnType<typeof vi.fn>;
let resolveGum: (s: unknown) => void;

beforeEach(() => {
  FakeMediaRecorder.instances = 0;
  vi.stubGlobal("MediaRecorder", FakeMediaRecorder);
  getUserMedia = vi.fn(() => new Promise((res) => {
    resolveGum = res as (s: unknown) => void;
  }));
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia },
  });
  // No isSecureContext stub needed: jsdom's default location is http://localhost,
  // and the hook only blocks getUserMedia when hostname !== "localhost".
});

// Regression: in continuous/hands-free mode the re-arm effect and a user mic-tap
// can both call start() during the ~100ms getUserMedia await, while React `state`
// is still "idle". Without a synchronous guard, the second call opens a second
// mic stream + recorder and orphans the first (mic stays live, interval leaks).
test("concurrent start() during the getUserMedia await creates only one recorder", async () => {
  const fakeStream = { getTracks: () => [] };
  const { result } = renderHook(() => useVoiceRecorder());

  await act(async () => {
    // Two starts before the permission promise resolves.
    result.current.start();
    result.current.start();
    resolveGum(fakeStream);
    await Promise.resolve();
    await Promise.resolve();
  });

  expect(getUserMedia).toHaveBeenCalledTimes(1);
  expect(FakeMediaRecorder.instances).toBe(1);
  expect(result.current.state).toBe("recording");
});

// Regression: unmounting the composer while a transcribe request is in flight
// must abort that request (don't leave the STT call running against a dead view).
test("unmounting mid-transcribe aborts the in-flight transcribe request", async () => {
  const fakeStream = { getTracks: () => [] };
  let fetchSignal: AbortSignal | undefined;
  const fetchMock = vi.fn((_url: string, opts: { signal?: AbortSignal }) => {
    fetchSignal = opts.signal;
    return new Promise<Response>(() => {}); // stays in-flight forever
  });
  vi.stubGlobal("fetch", fetchMock);

  const { result, unmount } = renderHook(() => useVoiceRecorder());

  await act(async () => {
    result.current.start();
    resolveGum(fakeStream);
    await Promise.resolve();
    await Promise.resolve();
  });
  expect(result.current.state).toBe("recording");

  // stop() → finalize() → reaches the (never-resolving) transcribe fetch.
  await act(async () => {
    void result.current.stop();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
  expect(fetchMock).toHaveBeenCalledTimes(1);
  expect(fetchSignal?.aborted).toBe(false);

  // Navigate away mid-transcribe.
  unmount();
  expect(fetchSignal?.aborted).toBe(true);
});
