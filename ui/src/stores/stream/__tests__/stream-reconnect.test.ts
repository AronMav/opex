import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { scheduleReconnect } from "../stream-reconnect";

function makeFakeSession() {
  return {
    write: vi.fn(),
    isCurrent: true,
    disposed: false,
    signal: new AbortController().signal,
    agent: "Arty",
    generation: 0,
    writeDraft: vi.fn(),
    dispose: vi.fn(),
  } as any;
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("scheduleReconnect", () => {
  it("bails out with error state once attempt >= maxAttempts", () => {
    const session = makeFakeSession();
    const resume = vi.fn();
    scheduleReconnect(session, "sid", 3, { resume, maxAttempts: 3, baseDelayMs: 500 });
    expect(resume).not.toHaveBeenCalled();
    expect(session.write).toHaveBeenCalledWith(
      expect.objectContaining({ connectionPhase: "error" })
    );
  });

  it("writes reconnecting state and schedules resume on attempt < max", () => {
    const session = makeFakeSession();
    const resume = vi.fn();
    scheduleReconnect(session, "sid", 0, { resume, maxAttempts: 6, baseDelayMs: 500 });
    expect(session.write).toHaveBeenCalledWith(
      expect.objectContaining({ connectionPhase: "reconnecting", reconnectAttempt: 1, maxReconnectAttempts: 6 })
    );
    vi.advanceTimersByTime(5000);
    expect(resume).toHaveBeenCalledWith(1);
  });

  it("syncs maxReconnectAttempts to deps.maxAttempts on first attempt (attempt=0)", () => {
    const session = makeFakeSession();
    const resume = vi.fn();
    // maxReconnectAttempts is only written on attempt=0 (constant — no need to re-sync)
    scheduleReconnect(session, "sid", 0, { resume, maxAttempts: 6, baseDelayMs: 100 });
    const writeCall = session.write.mock.calls.find((c: any[]) => c[0].connectionPhase === "reconnecting");
    expect(writeCall).toBeDefined();
    expect(writeCall![0].maxReconnectAttempts).toBe(6);
  });

  it("exponential backoff: delay >= baseDelayMs * 2^attempt * 0.8", () => {
    const session = makeFakeSession();
    const resume = vi.fn();
    // attempt=2 → baseline 500 * 4 = 2000ms, jitter ±20% → min ~1600ms
    scheduleReconnect(session, "sid", 2, { resume, maxAttempts: 5, baseDelayMs: 500 });
    vi.advanceTimersByTime(1599);
    expect(resume).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000); // total 2599ms — past +20% ceiling
    expect(resume).toHaveBeenCalled();
  });

  it("records timer handle via setTimer callback", () => {
    const session = makeFakeSession();
    const resume = vi.fn();
    const setTimer = vi.fn();
    scheduleReconnect(session, "sid", 0, { resume, maxAttempts: 3, baseDelayMs: 100, setTimer });
    expect(setTimer).toHaveBeenCalled();
    expect(setTimer.mock.calls[0][0]).not.toBeNull();
  });

  it("clears timer before invoking resume", () => {
    const session = makeFakeSession();
    const resume = vi.fn();
    const setTimer = vi.fn();
    scheduleReconnect(session, "sid", 0, { resume, maxAttempts: 3, baseDelayMs: 100, setTimer });
    vi.advanceTimersByTime(200);
    // setTimer called twice: once with handle, then null before resume
    expect(setTimer.mock.calls.length).toBeGreaterThanOrEqual(2);
    const lastCall = setTimer.mock.calls[setTimer.mock.calls.length - 1];
    expect(lastCall[0]).toBeNull();
    expect(resume).toHaveBeenCalled();
  });
});
