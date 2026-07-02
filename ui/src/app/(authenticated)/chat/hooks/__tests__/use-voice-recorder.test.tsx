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
  ondataavailable: ((e: unknown) => void) | null = null;
  onstop: (() => void) | null = null;
  constructor() {
    FakeMediaRecorder.instances++;
  }
  start() {}
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
  Object.defineProperty(window, "isSecureContext", {
    configurable: true,
    value: true,
  });
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
