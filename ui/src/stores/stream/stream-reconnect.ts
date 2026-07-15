// ── stream-reconnect.ts ─────────────────────────────────────────────────────
// Reconnect policy: exponential backoff with jitter, bounded by maxAttempts.
//
// Decouples the reconnect decision from streaming-renderer's internal state.
// Callers inject a resume callback (triggered on timer fire) and an optional
// setTimer callback (to manage the timer map held by the coordinator).
// Pure logic — suitable for unit testing with vi.useFakeTimers().

import type { StreamSession } from "../stream-session";

/**
 * Schedule a reconnect attempt with exponential backoff + jitter.
 *
 * When attempt >= maxAttempts, writes an error state to the session and
 * returns without scheduling anything. Otherwise writes reconnecting state,
 * computes a jittered delay, and schedules `deps.resume(attempt + 1)`.
 */
export function scheduleReconnect(
  session: StreamSession,
  sessionId: string,
  attempt: number,
  deps: {
    /** Called when the delay elapses — must trigger resumeStream(attempt + 1). */
    resume: (attempt: number) => void;
    /** Upper bound on attempt count — on reach, write error state and bail. */
    maxAttempts: number;
    /** Base delay in ms (will be exponentially scaled by attempt). */
    baseDelayMs: number;
    /** Optional: record the scheduled timer handle so coordinator can clear it on abort. */
    setTimer?: (handle: ReturnType<typeof setTimeout> | null) => void;
    /**
     * Called when maxAttempts is exhausted instead of writing connectionPhase="error".
     * Use this to fall back to history mode so the user sees the saved response
     * rather than a frozen partial stream.
     */
    onMaxAttemptsReached?: () => void;
  },
): void {
  if (attempt >= deps.maxAttempts) {
    deps.setTimer?.(null);
    if (deps.onMaxAttemptsReached) {
      deps.onMaxAttemptsReached();
    } else {
      session.write({
        streamError: "Connection lost after retries",
        connectionPhase: "error",
        connectionError: "Connection lost after retries",
      });
    }
    return;
  }

  session.write({
    // T7: "reconnecting" removed from the phase union — a pending backoff retry
    // now presents as "submitted" (still an active phase); reconnectAttempt +
    // isLlmReconnecting carry the retry-in-progress detail for the indicator.
    connectionPhase: "submitted",
    connectionError: null,
    reconnectAttempt: attempt + 1,
    // maxReconnectAttempts is a constant — only sync it on the first attempt
    ...(attempt === 0 && { maxReconnectAttempts: deps.maxAttempts }),
  });

  const baseDelay = deps.baseDelayMs * Math.pow(2, attempt);
  const jitter = baseDelay * 0.2 * (Math.random() * 2 - 1); // ±20%
  const delay = Math.max(0, baseDelay + jitter);

  const handle = setTimeout(() => {
    deps.setTimer?.(null);
    deps.resume(attempt + 1);
  }, delay);

  deps.setTimer?.(handle);
}
