# Message Order Stability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all four message ordering/visibility bugs that occur during and after SSE streaming: position jumping (a), flash disappearance (b/d), and duplicate bubbles (c).

**Architecture:** Introduce a `"finishing"` intermediate mode in `MessageSource` that holds the frozen live buffer visible while React Query refetches fresh DB data. The live→history transition becomes atomic from the user's perspective: the mode switches to `"history"` only when RQ has the fresh data in cache. Simultaneously, pre-allocate the assistant message UUID on the backend and send it in the `start` SSE event so that `mergeLiveOverlay`'s ID-based dedup works correctly.

**Tech Stack:** TypeScript/React (Zustand, TanStack Query), Rust (Axum SSE pipeline)

---

## Root Cause Analysis

### Symptom b/d — Flash (message disappears on stream finish)

`stream-processor.ts:468–477` switches `messageSource → "history"` then calls `invalidateQueries`. There is a render window where history mode is active but RQ cache still holds pre-stream data — the assistant response disappears until the refetch completes (200–500 ms).

### Symptom c — Duplicate bubbles

`StreamBuffer.reset()` generates a client-side UUID (`assistantId`). The DB assigns a different UUID on INSERT in `finalize.rs`. `mergeLiveOverlay`'s `historyIds.has(m.id)` check never matches → the live assistant message is appended after the identical history row, producing a duplicate that lasts until the live overlay is cleared.

### Symptom a — Position jumping

Live mode uses insertion order; history mode sorts by `created_at` (server timestamp). Any clock skew between client and server causes a visible reorder during the live→history transition.

### Legacy — `chat-overlay-dedup.ts`

137 lines of layered heuristics added as patches for each symptom: text-based user dedup (breaks when user sends same message twice), `lastHistAssistantTexts` preamble dedup (false positives), `agentId`-based continuation merge (breaks when `agentId` is `undefined` before `text-start`). These are replaced by clean ID-based dedup once IDs are synchronized.

---

## Files to Create / Modify

**Backend (Rust):**
- Modify: `crates/hydeclaw-core/src/agent/stream_event.rs` — add `message_id: Uuid` to `Start` variant
- Modify: `crates/hydeclaw-core/src/agent/pipeline/execute.rs` — pre-generate UUID, emit in `Start` event
- Modify: `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` — accept and use pre-generated UUID
- Modify: `crates/hydeclaw-core/src/gateway/handlers/chat.rs` — serialize `messageId` in SSE JSON

**Frontend (TypeScript):**
- Modify: `ui/src/stores/chat-types.ts` — add `"finishing"` to `MessageSource`
- Modify: `ui/src/stores/chat-selectors.ts` — handle `"finishing"` in `selectRenderMessages`
- Modify: `ui/src/stores/chat-overlay-dedup.ts` — replace with simple ID-based dedup (~25 lines)
- Modify: `ui/src/stores/stream/stream-processor.ts` — await RQ before mode switch; use `"finishing"` mode
- Modify: `ui/src/app/(authenticated)/chat/hooks/use-is-live.ts` — `"finishing"` ≠ live
- Modify: `ui/src/app/(authenticated)/chat/hooks/use-live-has-content.ts` — handle `"finishing"`
- Modify: `ui/src/app/(authenticated)/chat/hooks/use-is-replaying-history.ts` — `"finishing"` ≠ history
- Modify: `ui/src/stores/chat-history.ts` — `getLiveMessages` handles `"finishing"` mode

**Tests:**
- Modify: `ui/src/stores/__tests__/chat-overlay-dedup.test.ts` (or create if missing)
- Create: `ui/src/stores/__tests__/message-order-stability.test.ts`

---

## Task 1: Backend — Pre-allocate assistant message UUID

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/stream_event.rs`
- Modify: `crates/hydeclaw-core/src/agent/pipeline/execute.rs`
- Modify: `crates/hydeclaw-core/src/agent/pipeline/finalize.rs`
- Modify: `crates/hydeclaw-core/src/gateway/handlers/chat.rs`

- [ ] **Step 1: Read current `Start` variant in `stream_event.rs`**

Run: `grep -n "Start" crates/hydeclaw-core/src/agent/stream_event.rs | head -20`

Look for the `Start` variant definition and how it is serialized to JSON.

- [ ] **Step 2: Add `message_id` to `Start` variant**

In `stream_event.rs`, locate the `Start` variant. Add `message_id: uuid::Uuid` field:

```rust
Start {
    agent_name: Option<String>,
    message_id: uuid::Uuid,
},
```

The JSON serialization in `chat.rs` uses `match event { StreamEvent::Start { agent_name, .. } => ... }`. Update that match arm to also extract and emit `message_id`.

- [ ] **Step 3: Update `chat.rs` SSE serialization for `Start`**

Find the match arm that serializes `StreamEvent::Start`. Add `messageId` to the JSON payload:

```rust
StreamEvent::Start { agent_name, message_id } => {
    json!({
        "type": "start",
        "agentName": agent_name,
        "messageId": message_id.to_string(),
    })
}
```

- [ ] **Step 4: Read `execute.rs` to find where `Start` is emitted**

Run: `grep -n "Start\|message_id\|assistant" crates/hydeclaw-core/src/agent/pipeline/execute.rs | head -40`

Find exactly where `StreamEvent::Start { .. }` is constructed and emitted in the LLM loop.

- [ ] **Step 5: Pre-generate UUID in `execute.rs` and thread it through**

Before the LLM call / at the start of each tool-loop iteration, generate:
```rust
let assistant_message_id = uuid::Uuid::new_v4();
```

Include it in the `Start` event:
```rust
sink.emit(StreamEvent::Start {
    agent_name: Some(engine.cfg().agent.name.clone()),
    message_id: assistant_message_id,
}).await?;
```

Pass `assistant_message_id` to `finalize` (check current `finalize` signature to see how to thread it).

- [ ] **Step 6: Read `finalize.rs` to find the assistant message INSERT**

Run: `grep -n "insert\|message_id\|Uuid::new_v4\|assistant" crates/hydeclaw-core/src/agent/pipeline/finalize.rs | head -30`

Find where the assistant message row is created with a new UUID.

- [ ] **Step 7: Use pre-generated UUID in `finalize.rs`**

Replace `Uuid::new_v4()` (or equivalent) for the assistant message ID with the `assistant_message_id` that was threaded from `execute.rs`. The exact change depends on the current signature — ensure the pre-generated ID is passed into whatever `finalize` function creates the assistant row.

- [ ] **Step 8: Compile and verify**

Run: `make check`
Expected: no compile errors. Pay attention to any new `match` non-exhaustive errors from adding the `message_id` field to `Start`.

- [ ] **Step 9: Run Rust tests**

Run: `cargo test --package hydeclaw-core 2>&1 | tail -20`
Expected: all tests pass.

- [ ] **Step 10: Commit backend changes**

```bash
git add crates/hydeclaw-core/src/agent/stream_event.rs \
        crates/hydeclaw-core/src/agent/pipeline/execute.rs \
        crates/hydeclaw-core/src/agent/pipeline/finalize.rs \
        crates/hydeclaw-core/src/gateway/handlers/chat.rs
git commit -m "feat(sse): pre-allocate assistant message UUID; emit in Start event for ID sync"
```

---

## Task 2: Frontend — Add `"finishing"` mode to `MessageSource`

**Files:**
- Modify: `ui/src/stores/chat-types.ts`

- [ ] **Step 1: Add `"finishing"` variant to `MessageSource`**

In `chat-types.ts`, locate the `MessageSource` type (around line 148). Replace with:

```ts
export type MessageSource =
  | { mode: "new-chat" }
  | { mode: "live";      messages: ChatMessage[] }
  | { mode: "finishing"; sessionId: string; messages: ChatMessage[] }
  | { mode: "history";   sessionId: string };
```

- [ ] **Step 2: Update `getLiveMessages` helper**

`getLiveMessages` currently returns `source.mode === "live" ? source.messages : []`. Update it to also return messages from `"finishing"` mode (they are still "live" messages, just frozen):

```ts
export function getLiveMessages(source: MessageSource): ChatMessage[] {
  if (source.mode === "live") return source.messages;
  if (source.mode === "finishing") return source.messages;
  return [];
}
```

- [ ] **Step 3: Write failing tests for new mode**

In `ui/src/stores/__tests__/message-order-stability.test.ts` (create file):

```ts
import { describe, it, expect } from "vitest";
import { getLiveMessages } from "../chat-types";
import type { MessageSource } from "../chat-types";

describe("getLiveMessages", () => {
  it("returns messages from live mode", () => {
    const src: MessageSource = { mode: "live", messages: [{ id: "1", role: "user", parts: [] }] };
    expect(getLiveMessages(src)).toHaveLength(1);
  });

  it("returns messages from finishing mode", () => {
    const src: MessageSource = { mode: "finishing", sessionId: "s1", messages: [{ id: "2", role: "assistant", parts: [] }] };
    expect(getLiveMessages(src)).toHaveLength(1);
  });

  it("returns empty array for history mode", () => {
    const src: MessageSource = { mode: "history", sessionId: "s1" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });

  it("returns empty array for new-chat mode", () => {
    const src: MessageSource = { mode: "new-chat" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });
});
```

Run: `cd ui && npm test -- --run message-order-stability`
Expected: FAIL (getLiveMessages not updated yet) → then pass after Step 2.

- [ ] **Step 4: Run tests**

Run: `cd ui && npm test -- --run message-order-stability`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ui/src/stores/chat-types.ts ui/src/stores/__tests__/message-order-stability.test.ts
git commit -m "feat(chat-types): add 'finishing' mode to MessageSource for stable live→history transition"
```

---

## Task 3: Frontend — Update selectors and hooks for `"finishing"` mode

**Files:**
- Modify: `ui/src/stores/chat-selectors.ts`
- Modify: `ui/src/stores/chat-history.ts` (if `getCachedHistoryMessages` is called there)
- Modify: `ui/src/app/(authenticated)/chat/hooks/use-is-live.ts`
- Modify: `ui/src/app/(authenticated)/chat/hooks/use-live-has-content.ts`
- Modify: `ui/src/app/(authenticated)/chat/hooks/use-is-replaying-history.ts`

- [ ] **Step 1: Read current hook implementations**

Run: `cat ui/src/app/(authenticated)/chat/hooks/use-is-live.ts ui/src/app/(authenticated)/chat/hooks/use-live-has-content.ts ui/src/app/(authenticated)/chat/hooks/use-is-replaying-history.ts`

Note which modes they check. Each hook reads `messageSource.mode`.

- [ ] **Step 2: Write failing tests for hooks**

Add to `ui/src/stores/__tests__/message-order-stability.test.ts`:

```ts
import { selectIsLive, selectIsReplayingHistory, selectLiveHasContent } from "../chat-selectors";
import type { ChatState } from "../chat-types";

function makeState(mode: string, extra: Record<string, unknown> = {}): ChatState {
  return {
    agents: { Arty: { messageSource: { mode, ...extra }, selectedBranches: {} } },
    currentAgent: "Arty",
  } as unknown as ChatState;
}

describe("selectors with finishing mode", () => {
  it("selectIsLive returns false for finishing", () => {
    expect(selectIsLive(makeState("finishing", { sessionId: "s1", messages: [] }), "Arty")).toBe(false);
  });
  it("selectIsReplayingHistory returns false for finishing", () => {
    expect(selectIsReplayingHistory(makeState("finishing", { sessionId: "s1", messages: [] }), "Arty")).toBe(false);
  });
  it("selectLiveHasContent returns false for finishing (frozen, not streaming)", () => {
    expect(selectLiveHasContent(makeState("finishing", { sessionId: "s1", messages: [{ id: "1" }] }), "Arty")).toBe(false);
  });
});
```

Run: `cd ui && npm test -- --run message-order-stability`
Expected: FAIL

- [ ] **Step 3: Update `selectRenderMessages` in `chat-selectors.ts`**

Find `selectRenderMessages` (currently handles `"new-chat"`, `"history"`, `"live"`). Add `"finishing"`:

```ts
export function selectRenderMessages(state: ChatState, agent: string): ChatMessage[] {
  const st = state.agents[agent];
  if (!st) return [];
  const src = st.messageSource;
  if (src.mode === "new-chat") return [];
  if (src.mode === "history") {
    return getCachedHistoryMessages(src.sessionId, st.selectedBranches);
  }
  if (src.mode === "finishing") {
    // Show history (may be stale) merged with frozen live messages
    const history = getCachedHistoryMessages(src.sessionId, st.selectedBranches);
    return mergeLiveOverlay(history, src.messages);
  }
  // live mode
  const histSessionId = st.activeSessionId;
  const history = histSessionId ? getCachedHistoryMessages(histSessionId, st.selectedBranches) : [];
  return mergeLiveOverlay(history, src.messages);
}
```

Also update `selectIsLive` and `selectLiveHasContent`:
```ts
export function selectIsLive(state: ChatState, agent: string): boolean {
  return state.agents[agent]?.messageSource.mode === "live";  // finishing ≠ live
}
export function selectLiveHasContent(state: ChatState, agent: string): boolean {
  const src = state.agents[agent]?.messageSource;
  return src?.mode === "live" && src.messages.length > 0;  // finishing not counted
}
```

- [ ] **Step 4: Update hooks to match selectors**

Each hook delegates to the corresponding selector — verify they don't have their own `mode ===` checks. If they do, update to match the new semantics (`"finishing"` is not live, not history, not counted for liveHasContent).

- [ ] **Step 5: Run tests**

Run: `cd ui && npm test -- --run message-order-stability`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add ui/src/stores/chat-selectors.ts \
        ui/src/app/(authenticated)/chat/hooks/use-is-live.ts \
        ui/src/app/(authenticated)/chat/hooks/use-live-has-content.ts \
        ui/src/app/(authenticated)/chat/hooks/use-is-replaying-history.ts \
        ui/src/stores/__tests__/message-order-stability.test.ts
git commit -m "feat(selectors): handle 'finishing' mode in selectRenderMessages and derived selectors"
```

---

## Task 4: Frontend — Replace `mergeLiveOverlay` with ID-based dedup

**Files:**
- Modify: `ui/src/stores/chat-overlay-dedup.ts`

- [ ] **Step 1: Write failing tests for new dedup logic**

Create `ui/src/stores/__tests__/chat-overlay-dedup.test.ts` (check if it already exists):

```ts
import { describe, it, expect } from "vitest";
import { mergeLiveOverlay } from "../chat-overlay-dedup";
import type { ChatMessage } from "../chat-types";

function msg(id: string, role: "user" | "assistant", text = ""): ChatMessage {
  return { id, role, parts: text ? [{ type: "text", text }] : [] };
}

describe("mergeLiveOverlay", () => {
  it("returns history when live is empty", () => {
    const h = [msg("1", "user", "hi"), msg("2", "assistant", "hello")];
    expect(mergeLiveOverlay(h, [])).toEqual(h);
  });

  it("appends live messages not in history", () => {
    const h = [msg("1", "user", "hi")];
    const live = [msg("2", "assistant", "hello")];
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
    expect(result[1].id).toBe("2");
  });

  it("does NOT duplicate messages already in history by ID", () => {
    const h = [msg("1", "user", "hi"), msg("2", "assistant", "hello")];
    const live = [msg("1", "user", "hi"), msg("2", "assistant", "hello")];
    expect(mergeLiveOverlay(h, live)).toHaveLength(2);
  });

  it("filters empty assistant messages from live overlay", () => {
    const h = [msg("1", "user", "hi")];
    const live = [msg("2", "assistant", "")]; // empty parts
    expect(mergeLiveOverlay(h, live)).toHaveLength(1);
  });

  it("user sending same text twice: second live message is NOT deduplicated (ID-based)", () => {
    // The old text-based dedup would swallow this; new ID-based must not
    const h = [msg("1", "user", "да")];
    const live = [msg("99", "user", "да")]; // same text, different ID — NEW message
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
  });
});
```

Run: `cd ui && npm test -- --run chat-overlay-dedup`
Expected: some tests FAIL (old dedup logic has text-based dedup that breaks the last case)

- [ ] **Step 2: Replace `chat-overlay-dedup.ts` with simple ID-based dedup**

Replace the entire file content:

```ts
/**
 * Chat live-overlay dedup (Architecture C, simplified).
 *
 * History is React Query truth. Live is the SSE buffer (optimistic user
 * message + in-flight assistant). Merge rule: append live messages whose
 * ID is not yet in history, filtering empty assistant placeholders.
 *
 * ID-based dedup works correctly because the backend now pre-allocates the
 * assistant message UUID and sends it in the `start` SSE event. The live
 * buffer uses the same ID as the eventual DB row, so `historyIds.has(m.id)`
 * correctly detects when history has caught up.
 */

import type { ChatMessage } from "./chat-types";

export function mergeLiveOverlay(
  historyMessages: ChatMessage[],
  liveMessages: ChatMessage[],
): ChatMessage[] {
  if (liveMessages.length === 0) return historyMessages;

  const historyIds = new Set(historyMessages.map((m) => m.id));

  const extra = liveMessages.filter(
    (m) => !historyIds.has(m.id) && m.parts.length > 0,
  );

  return extra.length > 0 ? [...historyMessages, ...extra] : historyMessages;
}
```

- [ ] **Step 3: Run tests**

Run: `cd ui && npm test -- --run chat-overlay-dedup`
Expected: all PASS

- [ ] **Step 4: Run full UI test suite to catch regressions**

Run: `cd ui && npm test -- --run`
Expected: all tests pass (no regressions from removing old dedup heuristics)

- [ ] **Step 5: Commit**

```bash
git add ui/src/stores/chat-overlay-dedup.ts ui/src/stores/__tests__/chat-overlay-dedup.test.ts
git commit -m "refactor(overlay-dedup): replace 137-line heuristics with 10-line ID-based dedup"
```

---

## Task 5: Frontend — Await RQ refetch before mode switch in `stream-processor.ts`

**Files:**
- Modify: `ui/src/stores/stream/stream-processor.ts`

- [ ] **Step 1: Read the post-finally block in `stream-processor.ts`**

Read lines 466–479 of `ui/src/stores/stream/stream-processor.ts`. The current sequence is:
1. `session.write({ connectionPhase: "idle" })`
2. `callbacks.onStreamDone?.()`
3. Then (post-finally): `queryClient.invalidateQueries(sessions)`, `queryClient.invalidateQueries(sessionMessages)`, `session.write({ messageSource: { mode: "history" } })`

The race is between steps 2–3.

- [ ] **Step 2: Write a test for the `"finishing"` transition**

Add to `ui/src/stores/__tests__/message-order-stability.test.ts`:

```ts
import { getLiveMessages } from "../chat-types";
import type { MessageSource } from "../chat-types";

describe("finishing mode contract", () => {
  it("finishing mode holds messages while history is loading", () => {
    const liveMsg = { id: "live-1", role: "assistant" as const, parts: [{ type: "text" as const, text: "hello" }] };
    const src: MessageSource = { mode: "finishing", sessionId: "s1", messages: [liveMsg] };
    // Messages are still accessible during the finishing window
    expect(getLiveMessages(src)).toContainEqual(liveMsg);
  });

  it("history mode has no live messages (transition complete)", () => {
    const src: MessageSource = { mode: "history", sessionId: "s1" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });
});
```

Run: `cd ui && npm test -- --run message-order-stability`
Expected: PASS (getLiveMessages already handles "finishing" from Task 2)

- [ ] **Step 3: Update post-finally block in `stream-processor.ts`**

Find the post-finally block (after `if (!session.signal.aborted) {`). Currently it immediately switches to history mode. Replace with the `"finishing"` → await → `"history"` sequence:

```ts
// Post-finally: switch to finishing mode first, await RQ, then switch to history.
if (!session.signal.aborted) {
  if (receivedSessionId) {
    callbacks.onSessionId(receivedSessionId);
  }

  const completedSessionId = receivedSessionId ?? callbacks.getAgentState(agent)?.activeSessionId;

  if (completedSessionId) {
    // Step 1: freeze live messages into "finishing" mode so they stay visible
    const agentState = callbacks.getAgentState(agent);
    const currentLive =
      agentState?.messageSource.mode === "live" ? agentState.messageSource.messages : [];

    session.write({
      messageSource: { mode: "finishing", sessionId: completedSessionId, messages: currentLive },
    });

    // Step 2: invalidate sessions list (non-blocking — just fires off)
    queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });

    // Step 3: await the messages refetch so cache is fresh before switching to history.
    // refetchType: "active" (default) — only refetches if useSessionMessages is actively
    // subscribed in ChatThread. Since ChatThread keeps useSessionMessages mounted while
    // streaming, this is guaranteed to be active. If not subscribed, promise resolves
    // immediately (safe fallback: history shows stale data, same as current behavior).
    await queryClient.invalidateQueries({ queryKey: qk.sessionMessages(completedSessionId) });

    // Step 4: now it's safe to show history — RQ cache has the complete exchange
    session.write({ messageSource: { mode: "history", sessionId: completedSessionId } });
  } else {
    queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
  }
}
```

Note: `queryClient.invalidateQueries()` returns a `Promise<void>` that resolves when all active subscriptions for that key have been refetched. The `await` blocks only this async function — it does not block the React render thread.

- [ ] **Step 4: Run full UI test suite**

Run: `cd ui && npm test -- --run`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add ui/src/stores/stream/stream-processor.ts
git commit -m "fix(stream): await RQ refetch before switching to history; use 'finishing' mode to prevent flash"
```

---

## Task 6: Integration test and Pi deploy

- [ ] **Step 1: Build UI**

Run: `cd ui && npm run build 2>&1 | tail -10`
Expected: build succeeds, no type errors

- [ ] **Step 2: Run full test suite**

Run: `cd ui && npm test -- --run 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 3: Build Rust binary**

Run: `make check && make build-arm64 2>&1 | tail -10`
Expected: cross-compilation succeeds

- [ ] **Step 4: Deploy to Pi**

Run: `make deploy`
Expected: binary and UI deployed, service restarted

- [ ] **Step 5: Smoke test — start a stream, observe transition**

In the browser on the Pi, send a message and observe:
- No flash at stream finish (b/d fixed)
- No duplicate messages (c fixed)
- No position jumps (a fixed)
- ThinkingMessage appears during `submitted` phase
- Finishing mode shows message during RQ refetch window

- [ ] **Step 6: Commit any fixups**

If any visual regressions are found, fix and commit before marking complete.

---

## Success Criteria

1. No flash when stream completes: the assistant response stays visible during the RQ refetch window
2. No duplicate bubbles: live message and DB history row deduplicate correctly by ID
3. No position jumps: live→finishing→history transition is seamless in insertion order
4. ThinkingMessage still shows correctly (finishing mode ≠ active phase)
5. All existing UI tests pass
6. Rust compiles without warnings
