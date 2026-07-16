// ── stream/chat-stream.ts ────────────────────────────────────────────────────
// Client transport for the server-authoritative chat stream: startTurn =
// POST /api/chat (202), openTurnStream = GET /api/chat/{sid}/stream, which
// processes the sync_begin/replay/sync_end/live/finish envelope via
// processSSEStream (stream-processor.ts). Reconnect policy lives with the
// caller — this module never retries.
//
// Carry-forward from T4: `sync_begin.runStatus` can collapse an in-memory
// stream that actually ended in ERROR to "running"/"finished" in the
// active-stream branch — the real terminal `error` event is still in the
// replay buffer and is handled by the existing `error`/`sync` cases in
// stream-processor.ts. This module never treats `runStatus` as authoritative
// for error/interrupted UI state; it only uses it (inside stream-processor.ts)
// to decide onFinished vs. onConnectionLost when the connection closes
// without an explicit `finish`.

import { apiPost, assertToken, handleUnauthorized } from "@/lib/api";
import { useChatStore } from "../chat-store";
import { processSSEStream } from "./stream-processor";
import type { StreamSession } from "../stream-session";
import type { AgentState } from "../chat-types";

export interface TurnStreamCallbacks {
  /** Fired after `sync_end` — the replayed envelope has been committed as a single batch. */
  onEnvelopeApplied(): void;
  /** Fired when the turn is authoritatively over (finish / [DONE] / empty finished envelope). */
  onFinished(): void;
  /** Fired when the connection drops WITHOUT a terminal signal — caller decides whether to re-open. */
  onConnectionLost(): void;
  /** Fired on every parsed SSE event — drives the renderer's visibility-stale detector. */
  onEventActivity?(): void;
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
        callbacks: {
          // Required interface fields — no-ops / direct store reads. Safe to
          // wire directly to useChatStore here: chat-stream.ts is not part
          // of the chat-store → streaming-renderer → stream-processor cycle
          // that forced dependency-injection elsewhere.
          onSessionId: () => {},
          getAgentState: (a: string): AgentState | undefined => useChatStore.getState().agents[a],
          updateSessionParticipants: (sid: string, participants: string[]) =>
            useChatStore.getState().updateSessionParticipants(sid, participants),
          onEnvelopeApplied: () => cb.onEnvelopeApplied(),
          onFinished: () => cb.onFinished(),
          onConnectionLost: () => cb.onConnectionLost(),
          onEventActivity: () => cb.onEventActivity?.(),
        },
      });
    })
    .catch((err: unknown) => {
      if (err instanceof Error && err.name === "AbortError") return;
      cb.onConnectionLost();
    });
}
