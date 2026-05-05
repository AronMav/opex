# Message Order Stability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all four message ordering/visibility bugs (position jumping, flash disappearance, duplicates) by threading the assistant message UUID from SSE start through DB insert, adding a `"finishing"` intermediate mode to MessageSource, and replacing 137-line heuristic dedup with clean 15-line ID-based logic.

**Architecture:** Three independent changes compose to fix all symptoms. (1) Backend pre-generates assistant UUID in `execute.rs`, sends it in the existing `MessageStart` SSE event, and uses it in `finalize.rs` — no new Rust types required. (2) Frontend adds `"finishing"` mode to `MessageSource` that holds frozen live messages visible while React Query refetches. (3) `mergeLiveOverlay` is simplified to ID-only dedup, which works correctly once IDs match.

**Tech Stack:** Rust (Axum pipeline), TypeScript (Zustand, TanStack Query v5)

---

## File Map

**Create:**
- `ui/src/stores/__tests__/message-order-stability.test.ts`

**Modify:**
- `crates/hydeclaw-core/src/agent/pipeline/execute.rs` — change `msg_id` from `"msg_{uuid}"` string to real `Uuid`; add `assistant_message_id: Uuid` to `ExecuteOutcome`
- `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` — add `assistant_message_id: Uuid` to `FinalizeContext` and `finalize_context_from_engine`; use `save_message_ex_with_id` in `Done` branch
- `crates/hydeclaw-core/src/agent/engine/run.rs` — pass `outcome.assistant_message_id` at the 3 after-execute call sites; `Uuid::new_v4()` at the 3 slash-command call sites
- `ui/src/stores/chat-types.ts` — add `"finishing"` variant to `MessageSource`; update `getLiveMessages`
- `ui/src/stores/chat-selectors.ts` — handle `"finishing"` in `selectRenderMessages`, `selectIsLive`, `selectLiveHasContent`
- `ui/src/stores/chat-overlay-dedup.ts` — replace 137 lines with 15-line ID-based dedup
- `ui/src/stores/stream/stream-processor.ts` — post-finally: switch to `"finishing"`, await `refetchQueries`, then switch to `"history"`
- `ui/src/stores/__tests__/chat-overlay-dedup.test.ts` — rewrite for new ID-based semantics

**No changes needed:**
- `stream_event.rs` — already has `MessageStart { message_id: String }` ✓
- `gateway/handlers/chat.rs` — already emits `"messageId"` in the `start` SSE JSON ✓
- `use-is-live.ts`, `use-live-has-content.ts`, `use-is-replaying-history.ts` — pure selector delegates, no mode checks ✓

---

## Task 1: Backend — thread assistant UUID from execute to finalize

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/execute.rs`
- Modify: `crates/hydeclaw-core/src/agent/pipeline/finalize.rs`
- Modify: `crates/hydeclaw-core/src/agent/engine/run.rs`

### Context

Currently in `execute.rs:108`:
```rust
let msg_id = format!("msg_{}", Uuid::new_v4());
sink.emit(PipelineEvent::Stream(StreamEvent::MessageStart { message_id: msg_id }))
```
The `"msg_"` prefix means the frontend's `assistantId` (e.g. `"msg_550e8400-..."`) never matches the DB UUID (e.g. `"550e8400-..."`) saved later by `finalize.rs`. Fix: drop the prefix, add the UUID to `ExecuteOutcome`, and use `save_message_ex_with_id` in finalize.

`finalize_context_from_engine` is called 6 times in `run.rs` — 3 after `execute()` returns (lines ~159, ~275, ~376) and 3 on the slash-command path that skips `execute()` (lines ~135, ~253, ~354).

- [ ] **Step 1: Read the relevant execute.rs section**

Open `crates/hydeclaw-core/src/agent/pipeline/execute.rs` and locate:
- Line ~40: `ExecuteOutcome` struct definition (fields: `status`, `final_text`, `thinking_json`, `messages_len_at_end`, `final_parent_msg_id`)
- Line ~107: the `msg_id` generation + `MessageStart` emit

- [ ] **Step 2: Add `assistant_message_id` to `ExecuteOutcome`**

In `execute.rs`, find the `ExecuteOutcome` struct and add the new field:

```rust
pub struct ExecuteOutcome {
    pub status: ExecuteStatus,
    pub final_text: String,
    pub thinking_json: Option<serde_json::Value>,
    pub messages_len_at_end: usize,
    pub final_parent_msg_id: Uuid,
    /// UUID pre-generated for the final assistant DB row.
    /// Matches the `messageId` sent in the `MessageStart` SSE event
    /// so the frontend's live buffer ID equals the DB row ID.
    pub assistant_message_id: Uuid,
}
```

- [ ] **Step 3: Generate a real Uuid and use it in execute.rs**

Find lines ~107–110 in `execute.rs`:
```rust
let msg_id = format!("msg_{}", Uuid::new_v4());
match sink
    .emit(PipelineEvent::Stream(StreamEvent::MessageStart { message_id: msg_id }))
```

Replace with:
```rust
let assistant_msg_id = Uuid::new_v4();
match sink
    .emit(PipelineEvent::Stream(StreamEvent::MessageStart { message_id: assistant_msg_id.to_string() }))
```

- [ ] **Step 4: Add `assistant_message_id` to all `ExecuteOutcome` returns in execute.rs**

There are several early-return `ExecuteOutcome { ... }` constructions in `execute.rs`. For the bail-early return that happens BEFORE `assistant_msg_id` is generated (line ~97, the pre-loop `cancel.is_cancelled()` check), use `Uuid::nil()`. For all returns AFTER line ~108 (within or after the emit block), use `assistant_msg_id`.

Grep to find all `ExecuteOutcome {` in the file:
```
grep -n "ExecuteOutcome {" crates/hydeclaw-core/src/agent/pipeline/execute.rs
```

Add `assistant_message_id: uuid::Uuid::nil()` to the pre-loop early return (the one before `msg_id` is defined), and `assistant_message_id: assistant_msg_id` to all others.

Example for the nil case (pre-loop bail):
```rust
return Ok(ExecuteOutcome {
    status: ExecuteStatus::Interrupted("cancel_token"),
    final_text: String::new(),
    thinking_json: None,
    messages_len_at_end: messages.len(),
    final_parent_msg_id: last_msg_id,
    assistant_message_id: uuid::Uuid::nil(),
});
```

Example for all post-emit returns:
```rust
return Ok(ExecuteOutcome {
    status: ExecuteStatus::Interrupted("sink_closed"),
    final_text: String::new(),
    thinking_json: None,
    messages_len_at_end: messages.len(),
    final_parent_msg_id: last_msg_id,
    assistant_message_id: assistant_msg_id,
});
```

The final `return Ok(ExecuteOutcome { ... })` at the end of the loop also needs `assistant_message_id: assistant_msg_id`.

- [ ] **Step 5: Add `assistant_message_id` to `FinalizeContext` in finalize.rs**

Open `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` and find `pub struct FinalizeContext` (~line 289). Add the field:

```rust
pub struct FinalizeContext {
    pub db: PgPool,
    pub session_id: Uuid,
    pub agent_name: String,
    pub message_count: usize,
    pub provider: Arc<dyn LlmProvider>,
    pub memory_store: Arc<dyn MemoryService>,
    pub user_message_id: Option<Uuid>,
    pub ui_event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    pub max_iterations: usize,
    pub bg_tasks: Arc<TaskTracker>,
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
    pub compressor: crate::agent::compressor::Compressor,
    pub skill_review: Option<crate::config::SkillReviewConfig>,
    /// Pre-generated UUID for the final assistant message row.
    /// Matches the UUID sent in the `MessageStart` SSE event.
    pub assistant_message_id: Uuid,
}
```

- [ ] **Step 6: Add `assistant_message_id` to `finalize_context_from_engine`**

Find `pub fn finalize_context_from_engine` (~line 517) in `finalize.rs`. Add a parameter:

```rust
pub fn finalize_context_from_engine(
    engine: &crate::agent::engine::AgentEngine,
    session_id: Uuid,
    message_count: usize,
    user_message_id: Option<Uuid>,
    compressor: crate::agent::compressor::Compressor,
    assistant_message_id: Uuid,
) -> FinalizeContext {
    FinalizeContext {
        db: engine.cfg().db.clone(),
        session_id,
        agent_name: engine.cfg().agent.name.clone(),
        message_count,
        provider: engine.cfg().provider.clone(),
        memory_store: engine.cfg().memory_store.clone(),
        user_message_id,
        ui_event_tx: engine.state().ui_event_tx.clone(),
        max_iterations: engine.tool_loop_config().effective_max_iterations(),
        bg_tasks: engine.state().bg_tasks.clone(),
        llm_provider: Some(engine.cfg().provider.name().to_string()),
        llm_model: Some(engine.current_model()),
        compressor,
        skill_review: engine.cfg().agent.skill_review.clone(),
        assistant_message_id,
    }
}
```

- [ ] **Step 7: Use `save_message_ex_with_id` in the `Done` branch of finalize.rs**

Find the `FinalizeOutcome::Done { assistant_text, thinking_json }` match arm (~line 339). It currently calls:
```rust
sm.save_message_ex(
    ctx.session_id, "assistant", assistant_text,
    None, None, Some(agent_name_ref), thinking_json.as_ref(), ctx.user_message_id,
).await
```

Replace with a direct DB call (bypassing `SessionManager`) so we can supply the pre-allocated ID:
```rust
crate::db::sessions::save_message_ex_with_id(
    &ctx.db,
    ctx.assistant_message_id,
    ctx.session_id,
    "assistant",
    assistant_text,
    None,               // tool_calls
    None,               // tool_call_id
    Some(agent_name_ref),
    thinking_json.as_ref(),
    ctx.user_message_id,
).await
```

Leave the `Failed` and `Interrupted` branches unchanged — they still call `sm.save_message_ex(...)` without a pre-generated ID, because those are partial saves where ID sync does not matter (the stream has already ended in error or abort).

- [ ] **Step 8: Update all 6 call sites of `finalize_context_from_engine` in run.rs**

Open `crates/hydeclaw-core/src/agent/engine/run.rs`. There are 6 calls to `finalize_context_from_engine`. Run:
```
grep -n "finalize_context_from_engine" crates/hydeclaw-core/src/agent/engine/run.rs
```

For the 3 calls that come AFTER `execute()` returns (the lines directly after `let outcome = execute::execute(...).await?`), pass `outcome.assistant_message_id`:
```rust
let fin_ctx = finalize::finalize_context_from_engine(
    self,
    session_id,
    outcome.messages_len_at_end,
    Some(outcome.final_parent_msg_id),  // or whatever the current arg is
    compressor,
    outcome.assistant_message_id,       // ← new
);
```

For the 3 calls on the slash-command path (where `command_output.take()` is `Some` and `execute()` is NOT called), pass `Uuid::new_v4()` — no `MessageStart` was sent on this path, so ID sync is not needed:
```rust
let fin_ctx = finalize::finalize_context_from_engine(
    self,
    session_id,
    boot_for_execute.messages.len(),
    Some(user_message_id),
    compressor,
    uuid::Uuid::new_v4(),   // ← new; slash-command path, no MessageStart
);
```

- [ ] **Step 9: Compile**

```
make check
```

Expected: compilation succeeds. If there are `match non-exhaustive` or struct initialization errors, they will be on `ExecuteOutcome` constructions — add the missing `assistant_message_id` field to each.

- [ ] **Step 10: Run Rust tests**

```
cargo test --package hydeclaw-core 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 11: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/execute.rs \
        crates/hydeclaw-core/src/agent/pipeline/finalize.rs \
        crates/hydeclaw-core/src/agent/engine/run.rs
git commit -m "feat(pipeline): thread pre-allocated assistant UUID from execute to finalize for SSE-DB ID sync"
```

---

## Task 2: Frontend — add `"finishing"` mode to `MessageSource`

**Files:**
- Modify: `ui/src/stores/chat-types.ts`
- Modify: `ui/src/stores/chat-selectors.ts`
- Create: `ui/src/stores/__tests__/message-order-stability.test.ts`

### Context

`MessageSource` currently has three modes: `"new-chat"`, `"live"`, `"history"`. When the stream finishes, `stream-processor.ts` switches directly to `"history"` and then calls `invalidateQueries`. During the RQ refetch window, history mode shows stale cache — the assistant response disappears. Adding `"finishing"` as an intermediate step (live messages still visible, RQ refetch in progress) eliminates this flash. `"finishing"` is **not** an active phase (no ThinkingMessage, no streaming cursor).

- [ ] **Step 1: Write failing tests**

Create `ui/src/stores/__tests__/message-order-stability.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { getLiveMessages } from "@/stores/chat-types";
import { selectIsLive, selectIsReplayingHistory, selectLiveHasContent } from "@/stores/chat-selectors";
import type { MessageSource, ChatState } from "@/stores/chat-types";

// ── getLiveMessages ────────────────────────────────────────────────────────────

describe("getLiveMessages", () => {
  it("returns messages from live mode", () => {
    const src: MessageSource = { mode: "live", messages: [{ id: "1", role: "user", parts: [] }] };
    expect(getLiveMessages(src)).toHaveLength(1);
  });

  it("returns messages from finishing mode", () => {
    const src: MessageSource = {
      mode: "finishing",
      sessionId: "s1",
      messages: [{ id: "2", role: "assistant", parts: [] }],
    };
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

// ── Selectors: finishing mode is NOT live, NOT history ─────────────────────────

function fakeState(mode: string, extra: Record<string, unknown> = {}): ChatState {
  return {
    agents: {
      Arty: {
        messageSource: { mode, ...extra },
        selectedBranches: {},
        activeSessionId: null,
      },
    },
    currentAgent: "Arty",
  } as unknown as ChatState;
}

describe("selectors with finishing mode", () => {
  it("selectIsLive returns false for finishing", () => {
    const s = fakeState("finishing", { sessionId: "s1", messages: [] });
    expect(selectIsLive(s, "Arty")).toBe(false);
  });

  it("selectIsReplayingHistory returns false for finishing", () => {
    const s = fakeState("finishing", { sessionId: "s1", messages: [] });
    expect(selectIsReplayingHistory(s, "Arty")).toBe(false);
  });

  it("selectLiveHasContent returns false for finishing (frozen, not streaming)", () => {
    const s = fakeState("finishing", {
      sessionId: "s1",
      messages: [{ id: "x", role: "assistant", parts: [{ type: "text", text: "hi" }] }],
    });
    expect(selectLiveHasContent(s, "Arty")).toBe(false);
  });

  it("selectIsLive returns true for live mode", () => {
    const s = fakeState("live", { messages: [{ id: "y", role: "user", parts: [] }] });
    expect(selectIsLive(s, "Arty")).toBe(true);
  });
});

// ── finishing mode contract ────────────────────────────────────────────────────

describe("finishing mode contract", () => {
  it("holds messages while history is loading", () => {
    const liveMsg = {
      id: "live-1",
      role: "assistant" as const,
      parts: [{ type: "text" as const, text: "hello" }],
    };
    const src: MessageSource = { mode: "finishing", sessionId: "s1", messages: [liveMsg] };
    expect(getLiveMessages(src)).toContainEqual(liveMsg);
  });

  it("history mode has no live messages (transition complete)", () => {
    const src: MessageSource = { mode: "history", sessionId: "s1" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });
});
```

- [ ] **Step 2: Run tests — expect FAIL**

```
cd ui && npm test -- --run message-order-stability
```

Expected: FAIL — `"finishing"` is not a valid `MessageSource` mode yet.

- [ ] **Step 3: Add `"finishing"` to `MessageSource` in `chat-types.ts`**

Open `ui/src/stores/chat-types.ts`. Find the `MessageSource` type (~line 148):
```ts
export type MessageSource =
  | { mode: "new-chat" }
  | { mode: "live"; messages: ChatMessage[] }
  | { mode: "history"; sessionId: string };
```

Replace with:
```ts
export type MessageSource =
  | { mode: "new-chat" }
  | { mode: "live";      messages: ChatMessage[] }
  | { mode: "finishing"; sessionId: string; messages: ChatMessage[] }
  | { mode: "history";   sessionId: string };
```

- [ ] **Step 4: Update `getLiveMessages` in `chat-types.ts`**

Find `getLiveMessages` in `chat-types.ts`:
```ts
export function getLiveMessages(source: MessageSource): ChatMessage[] {
  return source.mode === "live" ? source.messages : [];
}
```

Replace with:
```ts
export function getLiveMessages(source: MessageSource): ChatMessage[] {
  if (source.mode === "live") return source.messages;
  if (source.mode === "finishing") return source.messages;
  return [];
}
```

- [ ] **Step 5: Update `selectRenderMessages`, `selectIsLive`, `selectLiveHasContent` in `chat-selectors.ts`**

Open `ui/src/stores/chat-selectors.ts`. Find `selectRenderMessages` and add the `"finishing"` branch:

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
    // History may be stale — show it merged with frozen live messages.
    // Once RQ refetch completes (Task 4), we switch to "history" mode.
    const history = getCachedHistoryMessages(src.sessionId, st.selectedBranches);
    return mergeLiveOverlay(history, src.messages);
  }
  // live mode
  const histSessionId = st.activeSessionId;
  const history = histSessionId ? getCachedHistoryMessages(histSessionId, st.selectedBranches) : [];
  return mergeLiveOverlay(history, src.messages);
}
```

Find `selectIsLive` and verify it only returns true for `"live"` (not `"finishing"`):
```ts
export function selectIsLive(state: ChatState, agent: string): boolean {
  return state.agents[agent]?.messageSource.mode === "live";
}
```

Find `selectLiveHasContent` and verify it only counts `"live"`:
```ts
export function selectLiveHasContent(state: ChatState, agent: string): boolean {
  const src = state.agents[agent]?.messageSource;
  return src?.mode === "live" && src.messages.length > 0;
}
```

If either already matches — no change needed. If they previously used `getLiveMessages(src).length > 0` which would now include `"finishing"`, update to the explicit mode check above.

- [ ] **Step 6: Run tests — expect PASS**

```
cd ui && npm test -- --run message-order-stability
```

Expected: all PASS.

- [ ] **Step 7: Run full test suite — no regressions**

```
cd ui && npm test -- --run 2>&1 | tail -20
```

Expected: all existing tests pass.

- [ ] **Step 8: Commit**

```bash
git add ui/src/stores/chat-types.ts \
        ui/src/stores/chat-selectors.ts \
        ui/src/stores/__tests__/message-order-stability.test.ts
git commit -m "feat(chat-types): add 'finishing' mode to MessageSource; update selectors"
```

---

## Task 3: Frontend — replace `mergeLiveOverlay` with ID-based dedup

**Files:**
- Modify: `ui/src/stores/chat-overlay-dedup.ts`
- Modify: `ui/src/stores/__tests__/chat-overlay-dedup.test.ts`

### Context

The current `chat-overlay-dedup.ts` is 137 lines with layered heuristics:
- Text-based user dedup (`historyUserTexts.has(firstText)`) — breaks when user sends same message twice
- `lastHistAssistantTexts` preamble dedup — false positives when model repeats a phrase
- `agentId`-based continuation merge — breaks when `agentId` is undefined (before `text-start` event)

With Task 1 complete, the live assistant ID now matches the DB row ID. ID-based dedup (`historyIds.has(m.id)`) works correctly and the heuristics can be deleted entirely.

- [ ] **Step 1: Rewrite `chat-overlay-dedup.test.ts`**

Replace the entire content of `ui/src/stores/__tests__/chat-overlay-dedup.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { mergeLiveOverlay } from "@/stores/chat-overlay-dedup";
import type { ChatMessage } from "@/stores/chat-types";

function msg(id: string, role: "user" | "assistant", text = ""): ChatMessage {
  return {
    id,
    role,
    parts: text ? [{ type: "text", text }] : [],
    createdAt: new Date().toISOString(),
  };
}

describe("mergeLiveOverlay — ID-based dedup", () => {
  it("returns history unchanged when live is empty", () => {
    const h = [msg("1", "user", "hi"), msg("2", "assistant", "hello")];
    expect(mergeLiveOverlay(h, [])).toEqual(h);
  });

  it("appends live messages not yet in history", () => {
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
    const live = [msg("2", "assistant", "")]; // parts is []
    expect(mergeLiveOverlay(h, live)).toHaveLength(1);
  });

  it("user sending same text twice — second message is NOT dropped (ID-based, not text-based)", () => {
    // Old text-based dedup would swallow this; new ID-based must not
    const h = [msg("1", "user", "да")];
    const live = [msg("99", "user", "да")]; // same text, different ID = new message
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2);
    expect(result[1].id).toBe("99");
  });

  it("optimistic user bubble (sending) stays visible before history catches up", () => {
    const live = [msg("u-opt", "user", "hello")];
    const result = mergeLiveOverlay([], live);
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("u-opt");
  });

  it("optimistic user bubble is removed once history has it (same ID)", () => {
    const h = [msg("u-opt", "user", "hello"), msg("a-1", "assistant", "reply")];
    const live = [msg("u-opt", "user", "hello")]; // same ID as history
    const result = mergeLiveOverlay(h, live);
    expect(result).toHaveLength(2); // deduplicated, not doubled
  });

  it("returns history reference unchanged when there are no extra live messages", () => {
    const h = [msg("1", "user", "hi")];
    const live = [msg("1", "user", "hi")]; // already in history
    const result = mergeLiveOverlay(h, live);
    expect(result).toBe(h); // same reference — no new array
  });
});
```

- [ ] **Step 2: Run tests — expect FAIL (old dedup logic)**

```
cd ui && npm test -- --run chat-overlay-dedup
```

Expected: FAIL — the "same text twice" case and the "same reference" case fail against the old 137-line implementation.

- [ ] **Step 3: Replace `chat-overlay-dedup.ts` with ID-based implementation**

Replace the entire content of `ui/src/stores/chat-overlay-dedup.ts`:

```ts
/**
 * Chat live-overlay dedup (Architecture C, simplified).
 *
 * History is React Query truth. Live is the SSE buffer (optimistic user
 * message + in-flight assistant). Merge rule: append live messages whose
 * ID is not yet in history, filtering empty assistant placeholders.
 *
 * ID-based dedup works because the backend now pre-allocates the assistant
 * message UUID and sends it in the `start` SSE event. The live buffer uses
 * the same ID as the eventual DB row, so `historyIds.has(m.id)` correctly
 * detects when history has caught up.
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

- [ ] **Step 4: Run tests — expect PASS**

```
cd ui && npm test -- --run chat-overlay-dedup
```

Expected: all PASS.

- [ ] **Step 5: Run full suite — no regressions**

```
cd ui && npm test -- --run 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add ui/src/stores/chat-overlay-dedup.ts \
        ui/src/stores/__tests__/chat-overlay-dedup.test.ts
git commit -m "refactor(overlay-dedup): replace 137-line heuristics with 15-line ID-based dedup"
```

---

## Task 4: Frontend — `"finishing"` transition in stream-processor + deploy

**Files:**
- Modify: `ui/src/stores/stream/stream-processor.ts`

### Context

The current post-finally block in `processSSEStream` (lines ~466–479):
```ts
// (post-finally)
if (!session.signal.aborted) {
  if (receivedSessionId) { callbacks.onSessionId(receivedSessionId); }
  queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
  const completedSessionId = receivedSessionId ?? ...;
  if (completedSessionId) {
    queryClient.invalidateQueries({ queryKey: qk.sessionMessages(completedSessionId) });
    session.write({ messageSource: { mode: "history", sessionId: completedSessionId } });
  }
}
```

The `session.write({ messageSource: "history" })` fires BEFORE the RQ refetch completes (200–500 ms window where history is stale). Fix: freeze live messages in `"finishing"` mode, call `refetchQueries` (which returns a Promise), await it, then switch to `"history"`.

`refetchQueries` is used instead of `invalidateQueries` for the messages query because `refetchQueries` always triggers a network request regardless of subscriber state. `invalidateQueries` with `refetchType: "active"` (default) resolves immediately if there are no active subscribers, which can happen during a tab switch.

- [ ] **Step 1: Read the post-finally block**

Open `ui/src/stores/stream/stream-processor.ts` and read from the `} finally {` block to the end of the file (~line 433 onward). Note the exact current sequence so you can replace it precisely.

- [ ] **Step 2: Add test for the finishing transition**

Add to `ui/src/stores/__tests__/message-order-stability.test.ts`:

```ts
// These are pure type/contract tests — the async transition itself
// is tested via integration (deploy + browser smoke test in Step 7).

import { getLiveMessages } from "@/stores/chat-types";
import type { MessageSource } from "@/stores/chat-types";

describe("finishing → history transition contract", () => {
  it("finishing mode preserves live messages while RQ refetches", () => {
    const liveMsg = {
      id: "db-uuid-123",
      role: "assistant" as const,
      parts: [{ type: "text" as const, text: "response" }],
    };
    const src: MessageSource = {
      mode: "finishing",
      sessionId: "sess-1",
      messages: [liveMsg],
    };
    expect(getLiveMessages(src)).toContainEqual(liveMsg);
  });

  it("history mode has empty live messages (transition complete)", () => {
    const src: MessageSource = { mode: "history", sessionId: "sess-1" };
    expect(getLiveMessages(src)).toHaveLength(0);
  });
});
```

Run: `cd ui && npm test -- --run message-order-stability`
Expected: PASS (getLiveMessages already updated in Task 2).

- [ ] **Step 3: Update the post-finally block in `stream-processor.ts`**

Find the block that starts with `// Post-finally:` (or similar comment) after the `finally { ... }` block. Replace the current sequence:

```ts
// Post-finally: switch to finishing mode first, await RQ refetch, then history.
if (!session.signal.aborted) {
  if (receivedSessionId) {
    callbacks.onSessionId(receivedSessionId);
  }

  const completedSessionId =
    receivedSessionId ?? callbacks.getAgentState(agent)?.activeSessionId;

  if (completedSessionId) {
    // Step 1: freeze live messages in "finishing" mode so they stay visible
    // while React Query fetches fresh data. The assistant response remains
    // on screen during the refetch window instead of flashing out.
    const agentState = callbacks.getAgentState(agent);
    const frozenLive =
      agentState?.messageSource.mode === "live"
        ? agentState.messageSource.messages
        : [];

    session.write({
      messageSource: {
        mode: "finishing" as const,
        sessionId: completedSessionId,
        messages: frozenLive,
      },
    });

    // Step 2: invalidate sessions list (non-blocking — just marks stale)
    queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });

    // Step 3: refetchQueries waits for the network request to complete
    // regardless of subscriber state. invalidateQueries with refetchType:"active"
    // (the default) resolves immediately if useSessionMessages is not mounted,
    // which can happen during a tab switch — making refetchQueries safer here.
    await queryClient.refetchQueries({
      queryKey: qk.sessionMessages(completedSessionId),
    });

    // Step 4: RQ cache now has the fresh exchange — safe to switch to history.
    // No flash: the assistant response was visible in "finishing" mode throughout.
    session.write({
      messageSource: { mode: "history" as const, sessionId: completedSessionId },
    });
  } else {
    queryClient.invalidateQueries({ queryKey: qk.sessions(agent) });
  }
}
```

Note: the `processSSEStream` function signature is already `async`, so `await` is valid. Remove the old `queryClient.invalidateQueries({ queryKey: qk.sessionMessages(...) })` call and the old `session.write({ messageSource: { mode: "history" } })` call so they are not duplicated.

- [ ] **Step 4: Run full UI test suite**

```
cd ui && npm test -- --run 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 5: Build UI**

```
cd ui && npm run build 2>&1 | tail -10
```

Expected: build succeeds with no TypeScript errors. Pay attention to any type errors on the new `"finishing"` variant — TypeScript exhaustiveness checks may surface them.

- [ ] **Step 6: Build ARM64 binary and deploy**

```
make check && make build-arm64 2>&1 | tail -10
make deploy
```

Expected: cross-compilation succeeds, binary and UI deployed to Pi, service restarted.

- [ ] **Step 7: Smoke test on Pi**

Open the chat UI in a browser connected to the Pi (`http://hydeclaw.local` or `http://192.168.1.82`). Send a message and observe:

1. **No flash** at stream finish — the assistant response stays visible continuously (fixes b/d)
2. **No duplicate bubbles** — the live assistant message and DB history row merge into one (fixes c)
3. **No position jumps** — message order is stable during the transition (fixes a)
4. **ThinkingMessage** still appears during `submitted` phase (finishing ≠ active phase)
5. **Session list** updates correctly after stream completes

- [ ] **Step 8: Commit**

```bash
git add ui/src/stores/stream/stream-processor.ts \
        ui/src/stores/__tests__/message-order-stability.test.ts
git commit -m "fix(stream): 'finishing' mode + await refetchQueries eliminates live→history flash"
```

---

## Success Criteria

1. No flash on stream finish — assistant response stays visible during the ~200–500 ms RQ refetch window
2. No duplicate assistant bubbles — DB UUID matches SSE `messageId`, ID-based dedup works
3. No position jumps — `finishing` mode preserves insertion order through the transition
4. ThinkingMessage still renders correctly (finishing is not an active phase)
5. All UI unit tests pass: `cd ui && npm test -- --run`
6. Rust compiles: `make check`
