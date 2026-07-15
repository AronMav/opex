// ── stream/chat-stream.ts ────────────────────────────────────────────────────
// T6: standalone client transport for the server-authoritative chat stream
// (T1-T5, deployed). NOT wired into the app yet — the cutover happens in T7.
//
// Two calls:
//   startTurn(agent, body)              → POST /api/chat, returns the 202 body
//   openTurnStream(agent, sid, session, cb) → GET /api/chat/{sid}/stream,
//     processes the sync_begin/replay/sync_end/live/finish envelope via the
//     existing processSSEStream in batch-apply mode (stream-processor.ts).
//
// No reconnect loop lives here: a drop without a terminal signal calls
// cb.onConnectionLost() and the caller (T7) decides whether to re-open.
//
// Carry-forward from T4: `sync_begin.runStatus` can collapse an in-memory
// stream that actually ended in ERROR to "running"/"finished" in the
// active-stream branch — the real terminal `error` event is still in the
// replay buffer and is handled by the existing `error`/`sync` cases in
// stream-processor.ts, unconditionally (not gated by batchMode). This module
// never treats `runStatus` as authoritative for error/interrupted UI state;
// it only uses it (inside stream-processor.ts) to decide onFinished vs.
// onConnectionLost when the connection closes without an explicit `finish`.

import { apiPost, assertToken, handleUnauthorized } from "@/lib/api";
import { useChatStore } from "../chat-store";
import { processSSEStream } from "./stream-processor";
import type { StreamSession } from "../stream-session";
import type { AgentState } from "../chat-types";

export interface TurnStreamCallbacks {
  /** Fired on `sync_begin` — resume boundary metadata (informational only). */
  onBoundary(boundaryMessageId: string | null, runStatus: string, truncated: boolean): void;
  /** Fired after `sync_end` — the replayed envelope has been committed as a single batch. */
  onEnvelopeApplied(): void;
  /** Fired when the turn is authoritatively over (finish / [DONE] / empty finished envelope). */
  onFinished(): void;
  /** Fired when the connection drops WITHOUT a terminal signal — caller decides whether to re-open. */
  onConnectionLost(): void;
}

/** POST /api/chat — starts (or continues) a turn. Returns the 202 body. */
export function startTurn(
  agent: string,
  body: Record<string, unknown>,
): Promise<{ session_id: string; user_message_id: string }> {
  return apiPost<{ session_id: string; user_message_id: string }>("/api/chat", {
    agent,
    ...body,
  });
}

/**
 * GET /api/chat/{sessionId}/stream — connects once, processes the envelope
 * (sync_begin → replay → sync_end → live → finish) via processSSEStream in
 * batch-apply mode, and dispatches `cb`. Does not retry or reconnect itself.
 */
export function openTurnStream(
  agent: string,
  sessionId: string,
  session: StreamSession,
  cb: TurnStreamCallbacks,
): void {
  const token = assertToken();

  fetch(`/api/chat/${sessionId}/stream?agent=${encodeURIComponent(agent)}`, {
    method: "GET",
    headers: { Authorization: `Bearer ${token}` },
    signal: session.signal,
  })
    .then((resp) => {
      if (resp.status === 401) {
        handleUnauthorized();
        return;
      }
      // No 204 branch: post-T4 the server always streams a sync_begin/sync_end
      // envelope (an already-concluded turn replays as an empty finished
      // envelope → onFinished), never a bare 204.
      if (!resp.ok) {
        return resp.text().then((t) => {
          throw new Error(t || `HTTP ${resp.status}`);
        });
      }
      return processSSEStream(session, resp.body!, {
        sessionId,
        reconnectAttempt: 0,
        batchMode: true,
        callbacks: {
          // Required legacy fields — no-ops / direct store reads. Safe to
          // wire directly to useChatStore here: chat-stream.ts is not part
          // of the chat-store → streaming-renderer → stream-processor cycle
          // that forced dependency-injection on the legacy path.
          onSessionId: () => {},
          onReconnectNeeded: () => {},
          getAgentState: (a: string): AgentState | undefined => useChatStore.getState().agents[a],
          updateSessionParticipants: (sid: string, participants: string[]) =>
            useChatStore.getState().updateSessionParticipants(sid, participants),
          onBoundary: (boundaryMessageId, runStatus, truncated) =>
            cb.onBoundary(boundaryMessageId, runStatus, truncated),
          onEnvelopeApplied: () => cb.onEnvelopeApplied(),
          onFinished: () => cb.onFinished(),
          onConnectionLost: () => cb.onConnectionLost(),
        },
      });
    })
    .catch((err: unknown) => {
      if (err instanceof Error && err.name === "AbortError") return;
      cb.onConnectionLost();
    });
}
