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

// ── Composer attachment (structural — ChatComposer's AttachmentEntry satisfies it) ─

/**
 * Attachment payload accepted by the composer actions. Only the fields the
 * streaming layer actually reads are required; ChatComposer.AttachmentEntry
 * (which also carries `id`, `file`, `uploadId`) is structurally assignable.
 */
export interface MessageAttachment {
  name: string;
  content: Array<{
    type: string;
    data: string;
    mimeType: string;
    filename?: string;
  }>;
}

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

export interface ClarifyPart {
  type: "clarify";
  clarifyId: string;
  question: string;
  choices: string[];
  timeoutMs: number;
  receivedAt: number;
  /** null = pending, string = response submitted */
  response: string | null;
}

export interface CompressionDividerPart {
  type: "compression-divider";
  segmentIndex: number;
  totalSegments: number;
}

export type MessagePart =
  | TextPart
  | ReasoningPart
  | FilePart
  | ToolPart
  | RichCardPart
  | ApprovalPart
  | ClarifyPart
  | CompressionDividerPart;

export interface ChatMessage {
  /**
   * Primary id — for assistants this is the FIRST DB row id when multiple
   * intermediate rows of one tool-loop turn are merged into a single visual
   * bubble (see convertHistory). For live ChatMessages, this is the row id
   * the streaming iteration will be persisted under.
   */
  id: string;
  /**
   * IDs of additional DB rows merged into this bubble (intermediate
   * iterations of the same tool-loop turn) — a provenance record of the rows
   * folded into one visual assistant bubble by convertHistory. Empty/absent
   * for non-merged messages.
   */
  mergedIds?: string[];
  role: "user" | "assistant";
  parts: MessagePart[];
  createdAt?: string;
  /** Per-message agent identity (for multi-agent sessions). */
  agentId?: string;
  /**
   * Lifecycle status of this message in the UI store.
   * - "sending" | "confirmed" | "failed": optimistic send status for the user
   *   message (SSE-03). "confirmed" lands once `data-session-id` acks it.
   * - "aborted": the assistant turn ended early (DB row status='aborted').
   * - "complete" | "streaming": set by the live `sync` SSE handler to mirror the
   *   backend run status for the assistant message (read by refetch logic, e.g.
   *   queries.ts uses "streaming" to poll). convertHistory does NOT set these —
   *   history rows map only "aborted" → "aborted".
   */
  status?: "sending" | "confirmed" | "failed" | "aborted" | "complete" | "streaming";
  /** Parent message ID in the tree (null for root/trunk messages). */
  parentMessageId?: string;
  /** The message this branch was forked from (set on fork-created user messages). */
  branchFromMessageId?: string;
  /** Reason the assistant stream ended early, if any (status === "aborted"). */
  abortReason?: string | null;
  /** True when this message was written by a cron delivery (session mirroring). */
  isMirror?: boolean;
  /** ISO timestamp if this message is bookmarked, null/undefined otherwise (T7). */
  bookmarkedAt?: string | null;
}

// ── Connection phase FSM (FSM-01) ────────────────────────────────────────────

/**
 * Single authoritative phase enum for stream lifecycle state.
 * FSM-01: authoritative connection phase enum.
 *
 * - "idle" — no turn in flight. Also the state a finished turn settles into;
 *   the boundary render keeps the completed assistant message visible, so
 *   there is no separate post-finish phase.
 * - "submitted" — a turn was POSTed and the client is waiting for/reading the
 *   envelope stream, before or between assistant text.
 * - "streaming" — the envelope has been applied and assistant output is live.
 * - "error" — the turn (or reconnect) ended in an unrecoverable error.
 *
 * A dropped connection does NOT get its own phase: it stays "submitted"
 * while the single connect path (`streaming-renderer.ts` `connect`) re-opens
 * the stream, gated by staleness/visibility checks. Whether that re-open is
 * in progress is tracked by the orthogonal `isLlmReconnecting` flag on
 * `AgentState`, not by `ConnectionPhase` itself.
 */
export type ConnectionPhase = "idle" | "submitted" | "streaming" | "error";

export function isActivePhase(phase: ConnectionPhase | undefined): boolean {
  return phase === "submitted" || phase === "streaming";
}

// ── MessageSource discriminated union (HIST-02) ─────────────────────────────

/**
 * Discriminated union for message source mode.
 * Replaces the dual-semantics of viewMode + liveMessages fields.
 * - "new-chat":   no session selected, no messages
 * - "live":       active or recently completed stream, messages held in store
 * - "finishing":  stream ended, frozen live messages visible while RQ refetches
 * - "history":    viewing a DB session snapshot, messages fetched via React Query
 */
export type MessageSource =
  | { mode: "new-chat" }
  | { mode: "live";      messages: ChatMessage[] }
  | { mode: "finishing"; sessionId: string; messages: ChatMessage[] }
  | { mode: "history";   sessionId: string };

/** Helper: extract live messages from a MessageSource union. */
export function getLiveMessages(source: MessageSource): ChatMessage[] {
  if (source.mode === "live") return source.messages;
  if (source.mode === "finishing") return source.messages;
  return [];
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
  /**
   * True while the LLM deadline retry loop is backing off before the next
   * attempt. Driven ONLY by the server's `reconnecting` SSE event (LLM-level
   * provider retry) — NOT the transport layer, which no longer reconnects (T8).
   */
  isLlmReconnecting: boolean;
  /** Branch selection state: parentMessageId -> selectedChildId. */
  selectedBranches: Record<string, string>;
  /**
   * Single-slot message queue. When the user presses Shift+Enter while streaming,
   * the message is stored here. A useEffect in ChatThread drains it when
   * connectionPhase transitions to idle (clean success only).
   * `voice: true` marks a message queued from a voice submit made while
   * streaming — ChatThread arms `voiceTurnPending` (below) when draining it so
   * the reply is spoken once the drained turn completes.
   *
   * Fix H: `sessionId` + `agent` stamp the target the message was queued FOR.
   * The drain (ChatThread) verifies the stamp still matches the current
   * agent/session before sending — otherwise it clears the item with a visible
   * notice, so a queued message is never silently lost NOR sent into the wrong
   * session (e.g. after switching agents or picking a different session).
   * Optional so hand-built test fixtures without a stamp keep the legacy
   * "always deliver" behaviour; `queueMessage` always writes both.
   */
  pendingMessage: {
    content: string;
    attachments?: Array<MessageAttachment>;
    voice?: boolean;
    sessionId?: string | null;
    agent?: string;
  } | null;
  /**
   * Single source of truth for "the turn that is about to start / just started
   * was voice-initiated" — set by a direct voice submit (ChatComposer, while
   * idle) or by ChatThread's pendingMessage drain (a queued voice message sent
   * after streaming ends). ChatComposer's spoken-reply effect reads and clears
   * it when the marked turn finishes, instead of relying on component-local
   * ref state (which can't be armed from ChatThread's drain effect).
   */
  voiceTurnPending?: boolean;
  /**
   * Input token count from the most recent LLM response (from the "usage" SSE event).
   * Only inputTokens is stored — outputTokens do not consume context window for display.
   * Null if no usage event has been received yet.
   */
  contextTokens: number | null;
  /**
   * Output token count from the most recent LLM response (from the "usage" SSE event).
   * Surfaced in the ContextBar tooltip breakdown. Null if no usage event yet.
   */
  contextOutputTokens: number | null;
  /**
   * Extended token breakdown from the most recent "usage" SSE event. All values
   * are SUBSETS of input/output (not additive) and may be null if the provider
   * did not report them. Used for tooltip display in ContextBar.
   */
  cacheReadTokens: number | null;
  cacheCreationTokens: number | null;
  reasoningTokens: number | null;
  /** True when there are older messages in DB not yet loaded (backward pagination). */
  hasMoreHistory: boolean;
  /** True while loadPreviousMessages() is fetching (prevents concurrent loads). */
  isLoadingHistory: boolean;
  /**
   * Real context window size (tokens) for the current model, received from the backend
   * via the data-session-id SSE event. Null until first session starts.
   * Single source of truth — replaces the static model-limits.ts table.
   */
  modelContextLimit: number | null;
  /** sync_begin.truncated — replay неполон (патологическое переполнение буфера); показываем баннер до конца хода. */
  replayTruncated: boolean;
}

// ── Store interface ─────────────────────────────────────────────────────────

export interface ChatStore {
  agents: Record<string, AgentState>;
  currentAgent: string;
  sessionParticipants: Record<string, string[]>;
  videoProgress: Record<string, { phase: string; text: string }>;

  setCurrentAgent: (name: string) => void;
  updateSessionParticipants: (sessionId: string, participants: string[]) => void;
  selectSession: (sessionId: string, forAgent?: string) => Promise<void>;
  selectSessionById: (agent: string, sessionId: string) => void;
  newChat: () => void;
  refreshHistory: (sessionId: string, agentName?: string) => void;
  clearError: () => void;

  sendMessage: (text: string, attachments?: Array<MessageAttachment>) => void;
  interruptAndSend: (text: string, attachments?: Array<MessageAttachment>) => Promise<void>;
  queueMessage: (text: string, attachments?: Array<MessageAttachment>, opts?: { voice?: boolean }) => void;
  clearPending: (agent?: string) => void;
  setVoiceTurnPending: (pending: boolean, agent?: string) => void;
  stopStream: () => void;
  regenerate: () => void;
  regenerateFrom: (messageId: string) => void;
  switchBranch: (parentMessageId: string, selectedChildId: string) => void;
  forkAndRegenerate: (messageId: string, newContent: string) => void;

  resumeStream: (agent: string, sessionId: string) => void;
  /**
   * Id-based live→history handoff. Called from ChatThread once the finished
   * turn's fresh rows land in the refetched sessionMessages cache (matched by
   * the live turn's assistant id): drops the frozen live overlay to history
   * mode. No-ops unless a finished turn is still shown as a live/finishing
   * overlay.
   */
  finalizeHandoff: (agent: string, sessionId: string) => void;
  setThinking: (agent: string, sessionId: string | null) => void;
  setThinkingLevel: (level: number) => void;
  markSessionActive: (agent: string, sessionId: string) => void;
  markSessionInactive: (agent: string, sessionId: string) => void;
  setVideoProgress: (sessionId: string, phase: string, text: string) => void;
  clearVideoProgress: (sessionId: string) => void;
  setModelOverride: (agent: string, model: string | null) => Promise<void>;
  renameSession: (sessionId: string, title: string) => Promise<void>;
  // skipInvalidation=true suppresses per-call cache invalidation for bulk-delete
  // so the caller can issue a single invalidation after all deletes complete.
  deleteSession: (sessionId: string, skipInvalidation?: boolean) => Promise<void>;
  deleteAllSessions: () => Promise<void>;
  deleteMessage: (messageId: string) => Promise<void>;
  loadEarlierMessages: (agent: string) => void;
  loadPreviousMessages: (agent: string) => Promise<void>;
  exportSession: () => Promise<void>;
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
    isLlmReconnecting: false,
    selectedBranches: {},
    pendingMessage: null,
    voiceTurnPending: false,
    contextTokens: null,
    contextOutputTokens: null,
    cacheReadTokens: null,
    cacheCreationTokens: null,
    reasoningTokens: null,
    hasMoreHistory: false,
    isLoadingHistory: false,
    modelContextLimit: null,
    replayTruncated: false,
  };
}
