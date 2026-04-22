// ── Helpers ──────────────────────────────────────────────────────────────────

/** Generate UUID v4 — crypto.randomUUID in secure contexts, fallback for plain HTTP */
export function uuid(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  // Fallback for non-secure contexts (HTTP, not HTTPS)
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    return (c === "x" ? r : (r & 0x3) | 0x8).toString(16);
  });
}

// ── Constants ────────────────────────────────────────────────────────────────

export const SESSIONS_PAGE_SIZE = 40;
export const MESSAGES_HISTORY_LIMIT = 100;
export const MAX_INPUT_LENGTH = 32_000;
export const STREAM_THROTTLE_MS = 50;

// ── Message types (replaces AI SDK UIMessage dependency) ────────────────────

export interface TextPart {
  type: "text";
  text: string;
}

export interface ReasoningPart {
  type: "reasoning";
  text: string;
}

export interface FilePart {
  type: "file";
  url: string;
  mediaType: string;
}

export interface SourceUrlPart {
  type: "source-url";
  url: string;
  title?: string;
}

export type ToolPartState =
  | "input-streaming"
  | "input-available"
  | "output-available"
  | "output-error"
  | "output-denied";

export interface ToolPart {
  type: "tool";
  toolCallId: string;
  toolName: string;
  state: ToolPartState;
  input: Record<string, unknown>;
  output?: unknown;
  errorText?: string;
}

export interface RichCardPart {
  type: "rich-card";
  cardType: string;
  data: Record<string, unknown>;
}

export interface StepGroupPart {
  type: "step-group";
  stepId: string;
  toolParts: ToolPart[];
  finishReason?: string;
  /** True while step is still receiving events */
  isStreaming: boolean;
}

export interface ApprovalPart {
  type: "approval";
  approvalId: string;
  toolName: string;
  toolInput: Record<string, unknown>;
  timeoutMs: number;
  receivedAt: number;
  status: "pending" | "approved" | "rejected" | "timeout_rejected";
  modifiedInput?: Record<string, unknown>;
}

export type MessagePart =
  | TextPart
  | ReasoningPart
  | FilePart
  | SourceUrlPart
  | ToolPart
  | RichCardPart
  | StepGroupPart
  | ApprovalPart;

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  parts: MessagePart[];
  createdAt?: string;
  /** Per-message agent identity (for multi-agent sessions). */
  agentId?: string;
  /** Optimistic send status (SSE-03). Undefined means confirmed (from history/sync). */
  status?: "sending" | "confirmed" | "failed" | "aborted";
  /** Parent message ID in the tree (null for root/trunk messages). */
  parentMessageId?: string;
  /** The message this branch was forked from (set on fork-created user messages). */
  branchFromMessageId?: string;
  /** Reason the assistant stream ended early, if any (status === "aborted"). */
  abortReason?: string | null;
}

// ── Connection phase FSM (FSM-01) ────────────────────────────────────────────

/**
 * Single authoritative phase enum for stream lifecycle state.
 * FSM-01: authoritative connection phase enum.
 * "complete" is a transient phase between finish event and finalizeStream.
 * "reconnecting" is set when stream drops mid-run and backoff retry is pending.
 */
export type ConnectionPhase = "idle" | "submitted" | "streaming" | "reconnecting" | "complete" | "error";

export function isActivePhase(phase: ConnectionPhase | undefined): boolean {
  return phase === "submitted" || phase === "streaming" || phase === "reconnecting";
}

// ── MessageSource discriminated union (HIST-02) ─────────────────────────────

/**
 * Discriminated union for message source mode.
 * Replaces the dual-semantics of viewMode + liveMessages fields.
 * - "new-chat": no session selected, no messages
 * - "live": active or recently completed stream, messages held in store
 * - "history": viewing a DB session snapshot, messages fetched via React Query
 */
export type MessageSource =
  | { mode: "new-chat" }
  | { mode: "live"; messages: ChatMessage[] }
  | { mode: "history"; sessionId: string };

/** Helper: extract live messages from a MessageSource union. */
export function getLiveMessages(source: MessageSource): ChatMessage[] {
  return source.mode === "live" ? source.messages : [];
}

// ── Per-agent state ─────────────────────────────────────────────────────────

export interface AgentState {
  activeSessionId: string | null;
  /** Discriminated union replacing the old liveMessages + viewMode duality. */
  messageSource: MessageSource;
  streamError: string | null;
  /** FSM-01: authoritative connection phase enum. */
  connectionPhase: ConnectionPhase;
  connectionError: string | null;
  /** When true, next sendMessage will force backend to create a new session. */
  forceNewSession: boolean;
  /** Server-driven list of session IDs currently being processed.
   *  Updated ONLY from WS agent_processing events — never optimistically.
   *  Array (not Set) because Immer doesn't support Set without enableMapSet(). */
  activeSessionIds: string[];
  /** How many messages to show at once (user can load more). */
  renderLimit: number;
  /** Per-session model override (null = use agent default). */
  modelOverride: string | null;
  /** Inline message when turn limit or cycle detection stops the loop. */
  turnLimitMessage: string | null;
  /** Per-agent stream generation counter (CLN-02 HIST-03) — detects stale SSE deltas. */
  streamGeneration: number;
  /** NET-02: Current reconnect attempt count (0 when not reconnecting). */
  reconnectAttempt: number;
  /** NET-02: Max reconnect attempts (exposed for UI indicator). */
  maxReconnectAttempts: number;
  /** True while the LLM deadline retry loop is backing off before next attempt. */
  isLlmReconnecting: boolean;
  /** Branch selection state: parentMessageId -> selectedChildId. */
  selectedBranches: Record<string, string>;
}

// ── Store interface ─────────────────────────────────────────────────────────

export interface ChatStore {
  agents: Record<string, AgentState>;
  currentAgent: string;
  sessionParticipants: Record<string, string[]>;

  setCurrentAgent: (name: string) => void;
  updateSessionParticipants: (sessionId: string, participants: string[]) => void;
  selectSession: (sessionId: string, forAgent?: string) => Promise<void>;
  selectSessionById: (agent: string, sessionId: string) => void;
  newChat: () => void;
  refreshHistory: (sessionId: string, agentName?: string) => void;
  clearError: () => void;

  sendMessage: (text: string, attachments?: Array<any>) => void;
  stopStream: () => void;
  regenerate: () => void;
  regenerateFrom: (messageId: string) => void;
  switchBranch: (parentMessageId: string, selectedChildId: string) => void;
  forkAndRegenerate: (messageId: string, newContent: string) => void;

  resumeStream: (agent: string, sessionId: string) => void;
  setThinking: (agent: string, sessionId: string | null) => void;
  setThinkingLevel: (level: number) => void;
  markSessionActive: (agent: string, sessionId: string) => void;
  markSessionInactive: (agent: string, sessionId: string) => void;
  setModelOverride: (agent: string, model: string | null) => Promise<void>;
  renameSession: (sessionId: string, title: string) => Promise<void>;
  deleteSession: (sessionId: string) => Promise<void>;
  deleteAllSessions: () => Promise<void>;
  deleteMessage: (messageId: string) => Promise<void>;
  loadEarlierMessages: (agent: string) => void;
  exportSession: () => Promise<void>;

  _selectCounter: Record<string, number>;
}

/** Alias for ChatStore — used in selector signatures for clarity. */
export type ChatState = ChatStore;

export function emptyAgentState(): AgentState {
  return {
    activeSessionId: null,
    messageSource: { mode: "new-chat" },
    streamError: null,
    connectionPhase: "idle",
    connectionError: null,
    forceNewSession: false,
    activeSessionIds: [],
    renderLimit: 100,
    modelOverride: null,
    turnLimitMessage: null,
    streamGeneration: 0,
    reconnectAttempt: 0,
    maxReconnectAttempts: 3,
    isLlmReconnecting: false,
    selectedBranches: {},
  };
}
