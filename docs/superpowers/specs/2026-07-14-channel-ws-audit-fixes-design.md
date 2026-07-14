# Channel WS Audit Fixes — Design

**Date:** 2026-07-14
**Status:** Approved design, pre-implementation
**Area:** `crates/opex-core/src/gateway/handlers/channel_ws/{reader,dispatcher,session_locks,writer,handshake,inline,mod,types}.rs`
**Source:** stability audit (multi-lens review + adversarial verification) — 7 confirmed defects.

## 1. Problem

A multi-lens stability audit of the per-connection channel WebSocket loop
(post-2026-05-06 reader/writer/dispatcher rework) surfaced 12 candidate defects;
adversarial verification confirmed 7 (4 refuted). Four are Important (two are
wedges/data-loss), three are Minor (one is a security inconsistency). This spec
fixes all seven.

## 2. Confirmed defects (fix targets)

| # | Sev | File | Defect |
|---|-----|------|--------|
| 1 | Important | dispatcher.rs:71 | Per-session FIFO not guaranteed — session lock acquired INSIDE the spawned task, so on the 4-worker runtime task2 can win the free lock before task1 → msg2 processed before msg1. |
| 2 | Important | handshake.rs:100 | Duplicate `Ready` re-runs `router.subscribe()` before the `take()` guard → registers a dead second subscription, overwrites `channel_conn_id`, leaves the first registered forever → `router.send().find()` can hit a dead id → channel actions silently lost until restart. |
| 3 | Important | reader.rs:115 | Reader `.await`s engine/DB work inline in callback interceptors (`approve_proposal` DB-tx, `router.send`, and `handle_clarify_text`'s `resolve_active_dm_session` DB query on EVERY plain-text message when a clarify is pending) → head-of-line stall, violating the "reader never awaits engine work" invariant. |
| 4 | Important | mod.rs:140 / writer.rs:29 | `writer::run`'s `sink.send().await` has no timeout; a stuck-but-open adapter blocks it → the bounded 256-slot `out_tx` fills → the reader blocks on its own `out_tx.send()` → stops reading inbound → ActionResult/Cancel frozen; wedge does not self-heal. |
| 5 | Minor(sec) | inline.rs:651 | `handle_clarify_text` resolves a clarify waiter with NO `is_owner` gate (unlike `handle_clarify_callback`); under `per-chat`/`shared` dm_scope any allowed non-owner member's next message resolves a clarify the agent directed at the owner. |
| 6 | Minor | reader.rs:101 | A `Message` arriving before `Ready`/`Config` is dispatched with `channel_type="unknown"` and no formatting prompt (no handshake-completion guard). |
| 7 | Minor | handshake.rs:90 | Repeated `Ready` pushes duplicate `connected_channels` rows (no dedup). |

## 3. Design

### 3.1 Per-session turn queue (fixes #1, #3-text, #6) — the core change

Replace `SessionLockMap` (`DashMap<SessionKey, Arc<Mutex<()>>>`) with a
`SessionQueueMap` (`DashMap<SessionKey, mpsc::UnboundedSender<QueuedTurn>>`),
same construction/scope as today's lock map.

`QueuedTurn` carries everything the turn needs (owned): `request_id`, `msg:
IncomingMessageDto`, `channel_type`, `formatting_prompt`, `timeout_secs`,
`out_tx`, `inflight` registry handle.

- **Enqueue (reader, in receive order):** `map.enqueue(key, turn)` gets-or-creates
  the per-key `UnboundedSender` and, on first creation, spawns a **consumer task**
  bound to that key's `Receiver`. `unbounded_send` is synchronous and never
  blocks the reader (turns are human-paced; a single user cannot realistically
  flood, and each turn is processed serially downstream). Ordering is guaranteed
  because a single `mpsc` receiver drains in send order and the reader sends in
  receive order.
- **Consumer task (serial per session):** `while let Some(turn) = rx.recv().await`
  → for each turn: **(a)** clarify-text check (moved off the reader — fix #3-text),
  owner-gated (fix #5); if it resolves a pending clarify, handle it and DO NOT run
  a turn; **(b)** otherwise register in `inflight` and run the engine turn (the
  current `dispatch_message` task body — status/chunk forwarders, timeout+grace,
  final frame, inflight removal). Serial processing = strict FIFO per session.
- **Consumer lifecycle:** spawned lazily on first turn for a key; exits when its
  `Receiver` closes (all senders dropped). Idle eviction removes the sender entry
  from the map (mirrors today's refcount eviction intent: an idle session's sender
  is dropped → the consumer drains any remainder and exits). On connection
  teardown the per-connection senders are dropped, so consumers wind down.
- **Cancel/inflight:** `request_id` is registered in `inflight` when the consumer
  **starts** a turn (not at enqueue). A `Cancel` for a still-queued (not-yet-
  started) turn is a no-op (rare; the turn runs shortly after). A `Cancel` for the
  running turn works exactly as today (cooperative token). This preserves the
  existing cooperative-cancel/finalize semantics.

**Fix #6 (handshake guard):** before enqueuing a `Message`, the reader rejects it
with an error frame when `state.channel_type == "unknown"` (Config not yet
received) — a `Message` must not create a bogus "unknown"-channel session.

### 3.2 Spawn callback interceptors (fix #3-callbacks)

The approval / initiative / infra / clarify-**callback** interceptors currently
`.await` engine/DB work inline in the reader (`approve_proposal` opens a DB
transaction; `send_cancel_button` awaits `router.send`). They resolve waiters
keyed by `request_id` and do not need per-session turn ordering. The reader
**spawns** each callback handler in its own task (off the reader hot path) rather
than awaiting it inline. The "was this consumed?" decision that currently gates
dispatch is preserved: callback request_ids are structurally distinct from turn
messages (they carry `iappr:`/`idismiss:`/`icancel:`/`infra:`/approval/clarify
callback-data prefixes), so the reader can classify a `Message` as
callback-vs-turn synchronously (cheap string-prefix inspection, no `.await`) and
route it: callback → spawn handler; plain turn → §3.1 enqueue. The one interceptor
that needs an async DB check to classify — clarify-**text** — is moved into the
consumer (§3.1), so the reader no longer awaits it.

### 3.3 Writer write-timeout (fix #4)

In `writer::run`, wrap each `sink.send(...).await` in
`tokio::time::timeout(WRITE_TIMEOUT, …)`. On timeout, log and `return` (exit the
writer) — the adapter is stuck. Exiting drops `out_rx`, so the reader's next
`out_tx.send()` returns `Err` → the reader breaks → normal connection teardown
(the existing disconnect path). This converts an indefinite wedge into a bounded
"close after `WRITE_TIMEOUT` of no write progress." `WRITE_TIMEOUT` is a module
const (e.g. 30s). Applies to both the `Wire` and `Ping` arms.

### 3.4 Subscribe under the take-guard (fix #2)

In `handshake::handle_ready`, move `router.subscribe()` INSIDE the
`action_install_tx.take()` guard so the router subscription (and the
`channel_conn_id` assignment) happens **only on the first `Ready`**. A second
`Ready` on the same connection then skips subscribe entirely (logged as
"Ready received twice"), so no dead second subscription is registered and the
first stays live and correctly unsubscribed on disconnect.

### 3.5 connected_channels dedup (fix #7)

In `handshake::handle_ready`, dedup the `connected_channels` push: only push a
`(agent_name, channel_type)` entry when no matching row exists (or fold it under
the same first-`Ready` `take()` guard as #2). A repeated `Ready` updates the
existing row's `last_activity` instead of pushing a duplicate.

### 3.6 clarify_text owner-gate (fix #5)

In the clarify-text resolution (now in the consumer, §3.1), add the same
`is_owner` check `handle_clarify_callback` performs before resolving the clarify
waiter (read `access_guards`, reject non-owners). A non-owner's plain-text
message in a shared/per-chat group session must NOT resolve a clarify the agent
directed at the owner; it falls through to a normal turn instead.

## 4. Data flow (Message path, after)

```text
reader (receive order, never awaits engine/DB):
  Message →
    if channel_type == "unknown": error frame, drop            (#6)
    else classify by callback-prefix (sync string check):
      callback (iappr:/idismiss:/icancel:/infra:/approval/clarify-cb)
        → tokio::spawn(handle_*_callback)                        (#3-callbacks)
      else (plain turn) → queue.enqueue(session_key, turn)       (#1, sync)

per-session consumer (serial, FIFO):
  turn →
    clarify-text pending & is_owner? resolve clarify, done       (#3-text, #5)
    else register inflight + run engine turn (timeout+grace)

writer: timeout(WRITE_TIMEOUT, sink.send) → Err/timeout → exit → teardown  (#4)
handshake handle_ready: subscribe + connected_channels push only on 1st Ready (#2,#7)
```

## 5. Error handling

- Enqueue after the consumer has exited (idle-evicted then a late turn) →
  get-or-create respawns a consumer; no message lost.
- Consumer panics in a turn → the consumer task ends; a fresh turn re-creates the
  consumer. (Turn bodies are already fail-soft via finalize; a panic is not
  expected.) Per-turn errors continue to be reported as `Error` frames as today.
- Writer timeout/exit is the intended teardown trigger (§3.3); the engine turn
  awaiting an action ack is already bounded by its own 10s `timeout` and recovers.
- Spawned callback handlers that error log and drop (as their inline versions do).

## 6. Testing

- **FIFO:** enqueue two turns for the same `SessionKey`; assert the consumer
  processes them in send order (instrument order via a recording sink / counter).
  Different keys still overlap.
- **Writer timeout:** a `CaptureSink` whose `poll_ready`/`start_send` blocks →
  `writer::run` returns within ~`WRITE_TIMEOUT` (use a short test const).
- **Subscribe-once:** two `Ready` frames → `router.subscribe` invoked once;
  `channel_conn_id` unchanged after the 2nd; `connected_channels` has one row.
- **clarify_text owner-gate:** non-owner plain text in a shared-scope session does
  NOT resolve a pending clarify (falls through to a turn); owner does resolve it.
- **handshake guard:** a `Message` before `Config` yields an error frame and no
  session dispatch.

## 7. Non-goals

- No change to the engine, finalize, cooperative-cancel/interrupt semantics, or
  the `IncomingMessage` contract — turn bodies move verbatim into the consumer.
- No bounded-queue backpressure/reject policy for the per-session queue
  (unbounded is safe for human-paced turns); revisit only if a flooding vector
  appears.
- No WS ping/pong liveness protocol (write-timeout is the wedge fix); the existing
  30s app-level ping stays.
- The 4 refuted candidates (Cancel double-frame, second-Ready replay, Pairing
  owner-check [the pairing code IS the authorization], duplicate-request_id
  [precondition unreachable]) are intentionally not addressed.
