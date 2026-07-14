# Channel WS Audit Fixes — Design

**Date:** 2026-07-14
**Status:** Approved design (revised post tri-review), pre-implementation
**Area:** `crates/opex-core/src/gateway/handlers/channel_ws/{reader,dispatcher,session_locks,writer,handshake,inline,mod,types}.rs`
**Source:** stability audit (multi-lens review + adversarial verification) — 7 confirmed defects. Revised after a 3-reviewer tri-review (concurrency / design-regression / correctness) that found the original per-session-consumer design deadlocked the mid-run clarify feature and left queue lifecycle races unspecified.

## 1. Problem

A multi-lens stability audit of the per-connection channel WebSocket loop
(post-2026-05-06 reader/writer/dispatcher rework) surfaced 12 candidate defects;
adversarial verification confirmed 7 (4 refuted). Four are Important (two are
wedges/data-loss), three are Minor (one is a security inconsistency).

The tri-review of the first design revision established two hard constraints the
final design must honour:

- **Moving clarify-text resolution INTO the strict-FIFO per-session consumer
  deadlocks the shipped mid-run clarify feature** (independently found by the
  concurrency and correctness reviewers). The engine turn that is waiting on a
  clarify answer holds the consumer task; the answer, enqueued behind it, can
  never be dequeued until the turn ends, but the turn only ends when the answer
  is processed. Clarify resolution MUST stay outside the FIFO boundary (as it is
  today).
- **The per-session queue's eviction has no passive `Arc::strong_count`-driven
  `Drop` moment** (unlike today's `SessionLockMap`). Any *active* idle-eviction
  introduces exactly the check-and-remove TOCTOU that `LockHandle::Drop`'s
  `remove_if`-under-shard-lock was written to avoid. The design removes active
  idle-eviction entirely (per-connection lifetime), sidestepping the race class.

## 2. Confirmed defects (fix targets)

| # | Sev | File | Defect |
|---|-----|------|--------|
| 1 | Important | dispatcher.rs:71 | Per-session FIFO not guaranteed — session lock acquired INSIDE the spawned task, so on the 4-worker runtime task2 can win the free lock before task1 → msg2 processed before msg1. |
| 2 | Important | handshake.rs:100 | Duplicate `Ready` re-runs `router.subscribe()` before the `take()` guard → registers a dead second subscription, overwrites `channel_conn_id`, leaves the first registered forever → `router.send().find()` can hit a dead id → channel actions silently lost until restart. |
| 3 | Important | reader.rs:115 | Reader `.await`s engine/DB work inline in callback interceptors (`approve_proposal` DB-tx, `router.send`, and `handle_clarify_text`'s `resolve_active_dm_session` DB query on EVERY plain-text message when a clarify is pending) → head-of-line stall, violating the "reader never awaits engine work" invariant. |
| 4 | Important | mod.rs:140 / writer.rs:29 | `writer::run`'s `sink.send().await` has no timeout; a stuck-but-open adapter blocks it → the bounded 256-slot `out_tx` fills → the reader blocks on its own `out_tx.send()` → stops reading inbound → ActionResult/Cancel frozen; wedge does not self-heal. |
| 5 | Minor(sec) | inline.rs:651 | `handle_clarify_text` resolves a clarify waiter with NO `is_owner` gate (unlike `handle_clarify_callback`); under `per-chat`/`shared` dm_scope any allowed non-owner member's next message resolves a clarify the agent directed at the owner. |
| 6 | Minor | reader.rs:101 | A `Message` arriving before the adapter's `Ready` is processed is dispatched with `channel_type="unknown"` and no formatting prompt (no handshake-completion guard). |
| 7 | Minor | handshake.rs:90 | Repeated `Ready` pushes duplicate `connected_channels` rows (no dedup). |

## 3. Execution batching

The tri-review converged on splitting execution by risk. **Batch A** (five
small, mutually-independent fixes) lands first as its own plan — no shared
state, no concurrency redesign, fast to review. **Batch B** (the queue
redesign: #1 + #3) lands second as its own plan, where all the concurrency
risk is concentrated and needs its own bake.

- **Batch A — small fixes:** #2, #4, #5, #6, #7.
- **Batch B — queue redesign:** #1 (FIFO queue), #3-text (clarify-text off the
  reader), #3-callbacks (spawn callbacks). Batch B RELOCATES the #5 owner-gate
  from its Batch-A in-place location into the spawned clarify-text task and must
  preserve it verbatim.

Both batches are specified in full below. The `writing-plans` step produces two
plans; `subagent-driven-development` executes Batch A, deploys/verifies, then
Batch B.

## 4. Batch A — small fixes

### 4.1 Subscribe under the take-guard (fix #2)

In `handshake::handle_ready`, move `router.subscribe()` INSIDE the
`action_install_tx.take()` guard so the router subscription (and the
`channel_conn_id` assignment) happens **only on the first `Ready`**. A second
`Ready` on the same connection then skips subscribe entirely (logged as
"Ready received twice"): `take()` returns `None`, subscribe is not called,
`channel_conn_id` is left untouched, and the oneshot handoff to the
action-forwarder (awaited in `mod.rs:168-171`) is unaffected since it only ever
fires once, guarded by the same `take()`. No dead second subscription is
registered; the first stays live and is correctly unsubscribed on disconnect.

### 4.2 Writer write-timeout (fix #4)

In `writer::run`, wrap each `sink.send(...).await` in
`tokio::time::timeout(WRITE_TIMEOUT, …)`. On timeout, log and `return` (exit the
writer) — the adapter is stuck. Exiting drops `out_rx`, so the reader's next
`out_tx.send()` returns `Err` → the reader breaks → normal connection teardown
(the existing disconnect path). This converts an indefinite wedge into a bounded
"close after `WRITE_TIMEOUT` of no write progress."

- `WRITE_TIMEOUT` is a module const pinned at **45s** (chosen above the existing
  30s app-level ping interval so a healthy-but-idle adapter mid-ping is never
  killed, and above the 20s tool-action grace). Applies to both the `Wire` and
  `Ping` arms.
- No extra trait bounds are needed: `tokio::time::timeout(_, sink.send(...))`
  compiles under the existing `S: Sink<WsMessage> + Unpin, S::Error: Display`.

### 4.3 clarify_text owner-gate (fix #5)

In `handle_clarify_text` (currently `inline.rs:651`, called inline from the
reader), add the same `is_owner` check `handle_clarify_callback` performs before
resolving the clarify waiter (read `access_guards`, reject non-owners). A
non-owner's plain-text message in a shared/per-chat group session must NOT
resolve a clarify the agent directed at the owner; it falls through to a normal
turn instead.

**Batch A lands this in place** (at the current inline call site). Batch B later
relocates `handle_clarify_text` into a spawned task (§5.3) and MUST carry this
owner-gate across verbatim.

### 4.4 Handshake-completion guard (fix #6)

In the reader's `Message` arm, before dispatching, reject the message with an
error frame when the adapter's `Ready` has not yet been processed — detected by
`state.channel_type == "unknown"` (its pre-`Ready` default; `channel_type` is
assigned at the top of `handle_ready`). A `Message` must not create a bogus
"unknown"-channel session with no formatting prompt. This guard is independent
of the queue and lands in Batch A; Batch B's reader keeps it ahead of the
classify/enqueue step (§5.4).

### 4.5 connected_channels dedup (fix #7)

In `handshake::handle_ready`, fold the `connected_channels` push under the same
first-`Ready` `take()`-guard used for #2 (single chosen behaviour — not an
either/or): push the `(agent_name, channel_type)` entry only on the first
`Ready`. A repeated `Ready` updates the existing row's `last_activity` via the
existing `.iter_mut().find(...)` path (`reader.rs:104-110`) instead of pushing a
duplicate. This also incidentally fixes the latent bug where `.find()` updated
only the first of several duplicate rows.

## 5. Batch B — per-session turn queue (fixes #1, #3)

### 5.1 SessionQueueMap

Replace `SessionLockMap` (`DashMap<SessionKey, Arc<Mutex<()>>>`) with a
`SessionQueueMap` holding `DashMap<SessionKey, mpsc::UnboundedSender<QueuedTurn>>`,
constructed per-connection in `channel_ws_loop` exactly where `lock_map` is today
(`mod.rs:141`).

`QueuedTurn` carries everything a turn needs to run without borrowing reader
state (all owned / cheaply-cloned): `request_id`, `msg: IncomingMessageDto`,
`channel_type`, `formatting_prompt`, `timeout_secs`, `out_tx`, `inflight`
(registry handle), `cancel_token: CancellationToken`, plus `ctx: CwsCtx`
(Arc-backed `Clone`), `engine: Arc<AgentEngine>`, and `agent_name: String` —
the last three are the params `dispatcher::dispatch_message` takes today and the
consumer needs them to run the turn body. `enqueue` therefore takes the turn
struct (which already carries these), unlike `SessionLockMap::acquire(key)`.

### 5.2 Enqueue, consumer, and lifecycle

- **Enqueue (reader, in receive order):** `map.enqueue(key, turn)` does
  `entry(key).or_insert_with(|| spawn_consumer())` to get-or-create the per-key
  `UnboundedSender`, then `send(turn)`. **The send Result is inspected:** on
  `Err` (receiver gone — a panicked/exited consumer whose `Sender` still sits in
  the map), the stale entry is removed, a fresh consumer is spawned, and the turn
  is re-sent. This closes the "panicked consumer blackholes the session" hole.
  `unbounded_send` is synchronous and never blocks the reader; ordering is
  guaranteed because a single `mpsc` receiver drains in send order and the reader
  sends in receive order.
- **Consumer task (serial per session):** `while let Some(turn) = rx.recv().await`
  → for each turn: if `turn.cancel_token.is_cancelled()` (cancelled while queued),
  skip it (matching today's "cancelled → Error frame" behaviour, §5.5); otherwise
  register nothing new (the request_id was registered at enqueue, §5.5) and run
  the engine turn — the current `dispatch_message` task body verbatim
  (status/chunk forwarders, timeout+grace, final frame, inflight removal). Serial
  await of the full body = strict FIFO per session. **The consumer captures only
  its own `Receiver` — never `Arc<SessionQueueMap>`** (a captured map clone would
  keep `strong_count > 0` and defeat teardown).
- **Lifecycle (no active idle-eviction):** the consumer exits strictly when
  `recv()` returns `None`, i.e. when all `Sender`s for its key are dropped. There
  is NO idle-timeout self-eviction — this deliberately removes the active
  check-and-remove TOCTOU. Entries live for the connection's lifetime; on
  connection teardown the per-connection `SessionQueueMap` is dropped, every
  `Sender` with it, and each consumer drains its remaining backlog and exits.
  Memory cost is one idle `Sender` + parked consumer task per distinct
  `SessionKey` seen on the connection (≈1 for a DM, one-per-active-chat for a
  group adapter) — bounded and small; accepted in exchange for eliminating the
  race class. Documented as the tradeoff vs today's refcount eviction.
- **Teardown / reconnect:** because the map is per-connection, a reconnecting
  adapter gets a fresh map + fresh consumers; the OLD connection's consumers may
  briefly still be draining. To bound this overlap, connection teardown cancels
  every request_id still registered in this connection's `inflight` (both running
  and still-queued turns — possible because cancel-tokens are registered at
  enqueue, §5.5). The old connection's `out_tx` is already gone, so any frame a
  lingering old consumer produces goes nowhere; its queued turns observe
  `is_cancelled()` and are skipped. A running old turn finishes under its own
  timeout+grace or is cancelled cooperatively — same bound as today.

### 5.3 clarify-text off the reader (fix #3-text, deadlock-safe)

Clarify-text resolution stays OUTSIDE the FIFO consumer (the tri-review
deadlock constraint). The reader gates it with a cheap synchronous check:

- Reader calls `clarify_mgr.has_any_pending()` (sync, no `.await`) for the Message.
- **No clarify pending (the common case):** the message is an ordinary turn →
  §5.4 classify + enqueue. Zero added latency, strict order preserved.
- **A clarify is pending:** the reader **spawns** a task (off its hot path) that
  does the async work today's inline path did — `resolve_active_dm_session(...)`,
  the #5 `is_owner` gate (relocated here verbatim from §4.3), and either resolves
  the matching clarify waiter OR, if this message resolves no waiter for this
  session, enqueues it as an ordinary turn via §5.4. This keeps the DB `.await`
  off the reader (fixes #3-text) and keeps clarify resolution off the FIFO
  consumer (no deadlock).
- Ordering note: the spawn path only runs while a clarify is outstanding for the
  session, where the user's next message is expected to BE the answer; a
  non-answer message sent in that narrow window may be enqueued after the async
  hop rather than strictly inline. This is an accepted, documented edge — it
  matches today's behaviour, which also resolves clarify-text out of band.

### 5.4 Spawn callback interceptors + sync classification (fix #3-callbacks)

The approval / initiative / infra / clarify-**callback** interceptors resolve
waiters keyed by `request_id` and need no per-session turn ordering, but today
`.await` engine/DB work inline in the reader (`approve_proposal` DB-tx,
`router.send`). The reader classifies each `Message` **synchronously** and routes:

- **Classification = `is_callback && known-prefix`** (both conditions, matching
  every interceptor's own gate: they check `msg.context["is_callback"] == true`
  first, then a `text` prefix). A plain chat message whose text merely looks like
  a prefix (e.g. `"infra:ok: yes"` typed by a user, `is_callback` absent/false)
  is NOT a callback → it falls through to enqueue as a turn. An `is_callback`
  message whose prefix matches none of the four known families also falls through
  to a turn (reproducing today's fallthrough at `reader.rs:147-158`). This
  prevents the "message vanishes" class the design reviewer flagged.
- **Callback** → `tokio::spawn(handle_*_callback)` (off the reader hot path).
- **Plain turn** → §5.1 enqueue.

Classification is a pure sync string/flag inspection — no `.await`. The one
interceptor that cannot classify synchronously (clarify-text, which needs a DB
lookup) is handled by the §5.3 `has_any_pending` gate, not here.

### 5.5 Cancel / inflight registration

- `request_id` is registered in `inflight` **at enqueue time** (in the reader),
  paired with the `QueuedTurn.cancel_token`. This is the fix for "Cancel of a
  still-queued turn is silently dropped": a `Cancel` arriving while the turn sits
  in the queue finds the registered token and cancels it; the consumer observes
  `is_cancelled()` and skips the turn, emitting the same "Cancelled" `Error`
  frame the running-turn cancel path emits today.
- A `Cancel` for the running turn works exactly as today (cooperative token +
  20s grace hard-abort backstop), unchanged.
- Teardown cancels all still-registered request_ids (§5.2).

## 6. Data flow (Message path, Batch B applied)

```text
reader (receive order, never awaits engine/DB):
  Message →
    if channel_type == "unknown": error frame, drop                     (#6)
    else if is_callback && known-prefix (sync flag+string check):
      → tokio::spawn(handle_*_callback)                                  (#3-callbacks)
    else if clarify_mgr.has_any_pending() (sync):
      → tokio::spawn(resolve_clarify_text | else enqueue as turn)        (#3-text, #5)
    else:
      register request_id+cancel_token in inflight; enqueue(session_key) (#1, sync)

per-session consumer (serial, FIFO, captures only its Receiver):
  turn →
    if cancel_token.is_cancelled(): skip (Cancelled frame)              (cancel-of-queued)
    else run engine turn verbatim (timeout+grace, final frame)

enqueue: entry.or_insert(spawn consumer); if send()==Err → evict+respawn+resend  (panic-safe)
teardown: drop map → senders drop → consumers drain+exit on recv()==None;
          cancel all inflight request_ids for this connection                    (double-run bound)

writer: timeout(45s, sink.send) → Err/timeout → exit → teardown        (#4)
handshake handle_ready: subscribe + connected_channels push only on 1st Ready (#2,#7)
```

## 7. Error handling

- **Enqueue into a dead consumer** (panicked/exited but `Sender` still mapped):
  `send()` returns `Err` → evict entry, respawn consumer, resend. No message lost.
- **Consumer panics mid-turn:** the task ends; the next enqueue for that key hits
  the `send()==Err` path above and respawns. Turn bodies are already fail-soft via
  finalize; per-turn errors keep surfacing as `Error` frames as today.
- **Cancel of a queued turn:** token registered at enqueue → consumer skips it →
  "Cancelled" frame. Cancel of a running turn: unchanged (cooperative + grace).
- **Teardown/reconnect overlap:** bounded by inflight-cancel (queued turns skipped,
  running turn cancelled/finishes under its timeout); old connection's frames are
  dropped (its `out_tx` is gone).
- **Writer timeout/exit** is the intended teardown trigger (§4.2); the engine turn
  awaiting an action ack is already bounded by its own 10s `timeout` and recovers.
- **Spawned callback / clarify-text handlers** that error log and drop, as their
  inline versions do.

## 8. Testing

**Batch A:**
- **Subscribe-once (#2):** two `Ready` frames → `router.subscribe` invoked once;
  `channel_conn_id` unchanged after the 2nd; `connected_channels` has one row (#7).
- **Writer timeout (#4):** a NEW test `Sink` impl whose `poll_ready`/`start_send`
  return `Poll::Pending` (the existing `CaptureSink` returns `Ready` and cannot
  simulate a stuck adapter — do not reuse it) → `writer::run` returns within
  ~`WRITE_TIMEOUT` using a short test-only const.
- **clarify_text owner-gate (#5):** non-owner plain text in a shared-scope session
  does NOT resolve a pending clarify (falls through to a turn); owner does resolve it.
- **Handshake guard (#6):** a `Message` before `Ready`/Config yields an error frame
  and no session dispatch.

**Batch B:**
- **FIFO (#1):** enqueue two turns for one `SessionKey`; assert the consumer
  processes them in send order (instrument via a recording sink / counter).
  Different keys overlap.
- **Callback-vs-lookalike routing (#3-callbacks):** an `is_callback` message with a
  known prefix routes to the spawned callback handler; a plain-text message whose
  text merely looks like a prefix (`is_callback` false) enqueues as a turn; an
  `is_callback` message with an unknown prefix enqueues as a turn.
- **Consumer respawn (panic-safe):** after a consumer's `Receiver` is gone, a fresh
  enqueue for the same key respawns a consumer and delivers the turn (assert the
  `send()==Err` → evict+respawn+resend path).
- **Cancel-of-queued:** enqueue a turn behind a long-running one, `Cancel` it while
  queued → it is skipped with a "Cancelled" frame and never runs.
- **Existing reader.rs `wire_guards` tests (reader.rs:238-278)** assert the textual
  ordering of interceptor calls in the reader source. Batch B moves clarify-text to
  a spawn and turns callbacks into spawns; these tests will break or go vacuous —
  **update them** to assert the new sync-classify + spawn/enqueue routing, not
  source-line ordering.

## 9. Non-goals / carried-forward limitations

- No change to the engine, finalize, or cooperative-cancel/interrupt semantics, or
  the `IncomingMessage` contract — turn bodies move verbatim into the consumer.
- No bounded-queue backpressure/reject policy for the per-session queue (unbounded
  is safe for human-paced turns); revisit only if a flooding vector appears.
- No active idle-eviction of queue entries (per-connection lifetime, §5.2) — a
  deliberate simplification to eliminate the check-and-remove TOCTOU.
- No WS ping/pong liveness protocol (write-timeout is the wedge fix); the existing
  30s app-level ping stays.
- **Cross-adapter serialization is NOT enforced** (pre-existing, not introduced
  here): `SessionQueueMap`, like `SessionLockMap` before it, is per-connection
  (`mod.rs:141`), so two adapters connected simultaneously to the same agent under
  `dm_scope="shared"` get independent maps for the same logical `SessionKey`. Called
  out so it is not mistaken for a new gap during review.
- The 4 refuted candidates (Cancel double-frame, second-Ready replay, Pairing
  owner-check [the pairing code IS the authorization], duplicate-request_id
  [precondition unreachable]) are intentionally not addressed.
