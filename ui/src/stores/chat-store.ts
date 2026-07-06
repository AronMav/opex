import { create } from "zustand";
import { devtools } from "zustand/middleware";
import { immer } from "zustand/middleware/immer";

import { queryClient } from "@/lib/query-client";

import type { ChatStore } from "./chat-types";
import { createStreamingRenderer } from "./streaming-renderer";
import type { StreamingRenderer } from "./streaming-renderer";
import { saveLastSession } from "./chat-persistence";
import { createNavigationActions } from "./chat/actions/navigation";
import { createStreamActions } from "./chat/actions/stream-control";
import { createSessionCrudActions } from "./chat/actions/session-crud";
import { createComposerActions } from "./chat/actions/composer";

// ── ActionDeps ──────────────────────────────────────────────────────────────
// Shared dependency bag passed to every action factory.
// Uses the same get/set closures that the immer factory provides — matching
// the existing codebase convention (no StoreApi adapter needed).
import type { QueryClient } from "@tanstack/react-query";

export type ActionDeps = {
  get: () => ChatStore;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  set: (updater: ((draft: any) => void) | Partial<ChatStore>) => void;
  queryClient: QueryClient;
  renderer: StreamingRenderer;
};

// ── Re-exports for backward compatibility ───────────────────────────────────
export type { ChatMessage, MessagePart, TextPart, ToolPart, ToolPartState, RichCardPart, FilePart, ReasoningPart, ConnectionPhase, MessageSource, ChatStore, ApprovalPart, ClarifyPart } from "./chat-types";
export { isActivePhase, MAX_INPUT_LENGTH, STREAM_THROTTLE_MS } from "./chat-types";
export { convertHistory, getCachedHistoryMessages, getCachedRawMessages, findSiblings } from "./chat-history";
export { saveLastSession, getInitialAgent, getLastSessionId } from "./chat-persistence";

// ── Store implementation ────────────────────────────────────────────────────

export const useChatStore = create<ChatStore>()(
  devtools(
    immer((set, get) => {
  // ── Streaming renderer (SSE processing, rAF throttling, reconnection) ──
  const renderer = createStreamingRenderer({ get, set });
  // Wire saveLastSession callback (avoids circular dependency)
  renderer.onSessionId((agent: string, sessionId: string) => {
    saveLastSession(agent, sessionId);
  });

  // ── Action factories ─────────────────────────────────────────────────────
  const navigationActions = createNavigationActions({ get, set, queryClient, renderer });
  const streamActions = createStreamActions({ get, set, queryClient, renderer });
  const sessionCrudActions = createSessionCrudActions({ get, set, queryClient, renderer });
  const composerActions = createComposerActions({ get, set, queryClient, renderer });

  return {
    agents: {},
    currentAgent: "",
    sessionParticipants: {},
    videoProgress: {},

    ...navigationActions,
    ...streamActions,
    ...sessionCrudActions,
    ...composerActions,

    setVideoProgress: (sessionId: string, phase: string, text: string) =>
      set((s) => { s.videoProgress[sessionId] = { phase, text }; }),
    clearVideoProgress: (sessionId: string) =>
      set((s) => { delete s.videoProgress[sessionId]; }),
  };
    }),
    { name: "ChatStore", enabled: process.env.NODE_ENV !== "production" },
  ),
);

