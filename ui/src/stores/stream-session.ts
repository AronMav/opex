"use client";

import { useChatStore } from "./chat-store";
import type { AgentState, ConnectionPhase, MessageSource } from "./chat-types";
import { StreamBuffer } from "./stream/stream-buffer";
import { STREAM_THROTTLE_MS } from "./chat-types";

export class StreamSession {
  readonly agent: string;
  readonly generation: number;
  readonly buffer: StreamBuffer;

  /**
   * Highest SSE event seq number processed so far. Set by stream-processor
   * from the standard `id:` field of every buffered event. Used as
   * `Last-Event-ID` header on reconnect/resume so the backend replays only
   * events the client hasn't seen — eliminates duplicate processing under
   * the dedup contract (Phase 3 offset tracking).
   */
  lastEventId: number | null = null;

  #controller: AbortController;
  #disposed = false;
  #updateTimer: ReturnType<typeof setTimeout> | null = null;
  #updateScheduled = false;

  constructor(agent: string, generation: number) {
    this.agent = agent;
    this.generation = generation;
    this.#controller = new AbortController();
    const currentAgent = useChatStore.getState().agents[agent];
    this.buffer = new StreamBuffer(currentAgent?.activeSessionId ?? null);
  }

  get signal(): AbortSignal {
    return this.#controller.signal;
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  get isCurrent(): boolean {
    if (this.#disposed) return false;
    const current = useChatStore.getState().agents[this.agent]?.streamGeneration ?? 0;
    return current === this.generation;
  }

  write(patch: Partial<AgentState>): void {
    if (!this.isCurrent) {
      if (process.env.NODE_ENV !== "production") {
        console.debug(
          `[StreamSession] dropped write for agent=${this.agent} gen=${this.generation}`,
          patch,
        );
      }
      return;
    }
    useChatStore.setState((draft) => {
      const st = draft.agents[this.agent];
      if (!st) return;
      Object.assign(st, patch);
    });
  }

  writeDraft(mutator: (agentDraft: AgentState) => void): void {
    if (!this.isCurrent) {
      if (process.env.NODE_ENV !== "production") {
        console.debug(
          `[StreamSession] dropped writeDraft for agent=${this.agent} gen=${this.generation}`,
        );
      }
      return;
    }
    useChatStore.setState((draft) => {
      const st = draft.agents[this.agent];
      if (!st) return;
      mutator(st);
    });
  }

  commit(phase?: ConnectionPhase): void {
    if (!this.isCurrent) return;
    this.writeDraft((agentDraft: AgentState) => {
      if (agentDraft.streamGeneration !== this.generation) return;
      // Resolve the live messages array, switching into a fresh live source
      // when the current mode isn't already live (preserves the original
      // switch-when-not-live semantics without `any` casts).
      const liveSrc: Extract<MessageSource, { mode: "live" }> =
        agentDraft.messageSource.mode === "live"
          ? agentDraft.messageSource
          : { mode: "live", messages: [] };
      const liveMessages = liveSrc.messages;
      const allParts = this.buffer.snapshot();
      const existingIdx = liveMessages.findIndex((m) => m.id === this.buffer.assistantId);
      if (existingIdx >= 0) {
        liveMessages[existingIdx].parts = allParts;
        liveMessages[existingIdx].agentId =
          this.buffer.currentRespondingAgent ?? undefined;
      } else {
        liveMessages.push({
          id: this.buffer.assistantId,
          role: "assistant",
          parts: allParts,
          createdAt: this.buffer.assistantCreatedAt,
          agentId: this.buffer.currentRespondingAgent ?? undefined,
        });
      }
      agentDraft.messageSource = liveSrc;
      const targetPhase = phase ?? "streaming";
      if (agentDraft.connectionPhase !== "error") {
        agentDraft.connectionPhase = targetPhase;
      }
    });
  }

  scheduleCommit(): void {
    if (this.#updateScheduled) return;
    this.#updateScheduled = true;
    this.#updateTimer = setTimeout(() => {
      this.#updateScheduled = false;
      this.#updateTimer = null;
      this.commit();
    }, STREAM_THROTTLE_MS);
  }

  cancelScheduledCommit(): void {
    if (this.#updateTimer !== null) {
      clearTimeout(this.#updateTimer);
      this.#updateTimer = null;
    }
    this.#updateScheduled = false;
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.cancelScheduledCommit();
    this.#controller.abort();
    useChatStore.setState((draft) => {
      const st = draft.agents[this.agent];
      if (!st) return;
      st.connectionPhase = "idle";
      st.streamGeneration = (st.streamGeneration ?? 0) + 1;
    });
  }
}

const activeSessions = new Map<string, StreamSession>();

export const streamSessionManager = {
  start(agent: string): StreamSession {
    const previous = activeSessions.get(agent);
    if (previous) {
      previous.dispose();
    } else {
      useChatStore.setState((draft) => {
        const st = draft.agents[agent];
        if (!st) return;
        st.streamGeneration = (st.streamGeneration ?? 0) + 1;
      });
    }

    const nextGen = useChatStore.getState().agents[agent]?.streamGeneration ?? 0;
    const session = new StreamSession(agent, nextGen);
    activeSessions.set(agent, session);
    return session;
  },

  current(agent: string): StreamSession | null {
    const s = activeSessions.get(agent);
    return s && !s.disposed ? s : null;
  },

  disposeCurrent(agent: string): void {
    const s = activeSessions.get(agent);
    if (!s) return;
    s.dispose();
    activeSessions.delete(agent);
  },
};
