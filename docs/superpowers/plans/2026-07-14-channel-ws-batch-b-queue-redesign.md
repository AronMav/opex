# Channel WS Audit Fixes — Batch B (per-session turn queue) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **PREREQUISITE: Batch A (`2026-07-14-channel-ws-batch-a-small-fixes.md`) must be implemented, reviewed, and deployed BEFORE this plan starts.** Batch B rewrites `reader.rs` and must incorporate Batch A's #6 handshake guard (already in `reader.rs` by then). If Batch A is not yet merged, stop and execute it first.

**Goal:** Replace the per-`SessionKey` mutex with a per-session FIFO turn queue so same-session messages are processed in strict receive order (#1), and move all engine/DB `.await`s off the reader hot path via sync classification + spawned handlers (#3), WITHOUT deadlocking the mid-run clarify feature.

**Architecture:** A `SessionQueueMap` (`DashMap<SessionKey, mpsc::UnboundedSender<QueuedTurn>>`) replaces `SessionLockMap`. The reader classifies each `Message` synchronously and either spawns a callback handler (off the hot path), spawns a short-lived clarify-text resolver (only when a clarify is pending — the deadlock-safe alternative to running it in the FIFO consumer), or enqueues an ordinary turn. A lazily-spawned per-key consumer task drains its queue serially, running the (verbatim-moved) turn body. Consumers exit on `recv()==None`; there is NO active idle-eviction (per-connection lifetime), which removes the check-and-remove TOCTOU class. Cancel tokens are registered at enqueue so a Cancel for a still-queued turn is honoured.

**Tech Stack:** Rust 2024, tokio (`mpsc::unbounded_channel`, `tokio::spawn`, `AbortHandle`, `CancellationToken`), `dashmap`, axum WS. rustls-tls only — no new deps.

## Global Constraints

- Rust + rustls-tls only — no new external dependency.
- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/`.
- Do NOT push; do NOT deploy — controller runs the server test session + deploy after review, on explicit user approval.
- Windows dev host cannot run the Rust suite reliably — authority is the server (`~/opex-src`, throttled `CARGO_BUILD_JOBS=4 nice ionice`). Local `cargo check`/`clippy` only.
- **Reader never awaits engine/DB work.** Every `Message` path must be sync-decide-then-spawn-or-enqueue. The ONLY awaits left in the reader's Message arm are non-engine `out_tx.send(...)` (frame emission) — the same kind already there for invalid-JSON and the #6 guard.
- **Clarify resolution MUST stay outside the FIFO consumer** (tri-review deadlock constraint). The consumer runs turn bodies only.
- **The consumer task MUST NOT capture `Arc<SessionQueueMap>`** — it holds only its `Receiver`. A captured map clone keeps a strong ref alive and prevents teardown.
- **Classification = `is_callback && known-prefix`.** A plain-text message that merely looks like a prefix (`is_callback` false/absent) is a turn. An `is_callback` message with no known prefix is a turn (reproduces today's fallthrough). Never route a plain message to a callback handler.
- Preserve R-CHANNEL cooperative-cancel semantics: a running turn is cancelled via its token (finalize → `interrupted`), never hard-aborted while healthy. Hard-abort remains ONLY a post-grace backstop for a sync-wedged turn.
- Source spec: `docs/superpowers/specs/2026-07-14-channel-ws-audit-fixes-design.md` §5.

## File Structure

- `session_queue.rs` (NEW, replaces `session_locks.rs`) — `SessionKey` re-exported from `types.rs`, `QueuedTurn`, `SessionQueueMap`, `enqueue`, the consumer + `run_turn_body`, and `cancel`. Owns all per-session serialisation + turn execution.
- `session_locks.rs` — DELETED.
- `dispatcher.rs` — DELETED (its `dispatch_message` becomes `enqueue`; its `cancel` moves to `session_queue.rs`).
- `types.rs` — `InflightMessage` changes from `{ join_handle, cancel }` to `{ cancel, abort: Option<AbortHandle> }`.
- `inline.rs` — add sync `*_matches` classifiers, refactor the four callback handlers' preambles to call them (zero prefix-knowledge duplication).
- `reader.rs` — Message arm rewired to sync-classify → spawn/enqueue; wire_guards tests updated.
- `mod.rs` — construct `SessionQueueMap`; teardown cancels inflight tokens + grace + abort.

---

### Task 1: `InflightMessage` shape + `SessionQueueMap` core

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/types.rs:83-97` (`InflightMessage`)
- Create: `crates/opex-core/src/gateway/handlers/channel_ws/session_queue.rs`
- Delete: `crates/opex-core/src/gateway/handlers/channel_ws/session_locks.rs`
- Delete: `crates/opex-core/src/gateway/handlers/channel_ws/dispatcher.rs`
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/mod.rs:19-25` (module decls)
- Test: inline `#[cfg(test)]` in `session_queue.rs`

**Interfaces:**
- Consumes: `SessionKey` (from `types.rs`), `InflightRegistry = Arc<Mutex<HashMap<String, InflightMessage>>>`, `OutboundMsg`, `IncomingMessageDto`, `AgentEngine`.
- Produces:
  - `pub(super) struct InflightMessage { pub cancel: CancellationToken, pub abort: Option<tokio::task::AbortHandle> }`
  - `pub(super) struct QueuedTurn { engine, agent_name, channel_type, formatting_prompt, request_id, msg, timeout_secs, out_tx, inflight, cancel_token }`
  - `pub(super) struct SessionQueueMap` with `pub fn new() -> Arc<Self>` and `pub async fn enqueue(self: &Arc<Self>, key: SessionKey, turn: QueuedTurn)`
  - `pub(super) async fn cancel(request_id: &str, inflight: &InflightRegistry) -> bool`

**Background:** In the mutex model each turn was a `tokio::spawn`ed task and `InflightMessage` held its `JoinHandle`. In the queue model, turns run inside the per-session consumer. To keep the FIFO guarantee the consumer awaits each turn serially; to keep the sync-wedge hard-abort backstop it spawns the turn body and stores the task's `AbortHandle` in the inflight entry. The cancel token is registered at ENQUEUE (in the reader, Task 3), so `abort` starts `None` and is filled once the consumer spawns the turn.

- [ ] **Step 1: Change `InflightMessage`**

In `types.rs`, replace the struct (lines 83-92) with:

```rust
/// One in-flight message tracked so a `Cancel` for ANY request_id can stop it.
/// Registered at ENQUEUE with `abort = None`; the per-session consumer fills
/// `abort` once it spawns the turn body, enabling a post-grace hard-abort of a
/// sync-wedged turn without killing the consumer (which serves the whole
/// session's queue).
pub(super) struct InflightMessage {
    /// Per-turn cooperative cancellation token wired into `handle_with_status`.
    /// R-CHANNEL: cancelling stops the turn COOPERATIVELY (finalize →
    /// 'interrupted'), not a hard abort (which guard-drops to 'failed').
    pub cancel: CancellationToken,
    /// Abort handle for the spawned turn task; `None` while the turn is still
    /// queued. Used only as a post-grace backstop for a sync-wedged turn.
    pub abort: Option<tokio::task::AbortHandle>,
}
```

- [ ] **Step 2: Swap module declarations**

In `mod.rs`, replace lines 19-25:

```rust
mod handshake;
mod inline;
mod reader;
mod session_queue;
mod types;
mod writer;
```

(Removes `mod dispatcher;` and `mod session_locks;`, adds `mod session_queue;`.)

- [ ] **Step 3: Delete the old files**

```bash
git rm crates/opex-core/src/gateway/handlers/channel_ws/session_locks.rs
git rm crates/opex-core/src/gateway/handlers/channel_ws/dispatcher.rs
```

- [ ] **Step 4: Write `session_queue.rs` — struct + enqueue + consumer + turn body**

Create the file. The `run_turn_body` fn is the OLD `dispatcher.rs` spawned-task body (dispatcher.rs lines 73-166) moved verbatim EXCEPT: (a) it no longer acquires a session lock (the `let _lock = lock_map.acquire(session_key).await;` line is dropped — serialisation now comes from the consumer awaiting turns in order), and (b) it does NOT remove itself from `inflight` (the consumer removes after the awaited turn completes). Everything else — the chunk/phase forwarders, the timeout+20s-grace+`drop(fut)` engine call, the final `Done`/`Error` frame — is unchanged.

```rust
//! Per-session FIFO turn queue. Replaces the per-`SessionKey` mutex.
//!
//! A `SessionQueueMap` holds one `mpsc::UnboundedSender<QueuedTurn>` per active
//! `SessionKey`. The reader enqueues turns in receive order; a lazily-spawned
//! consumer task drains its queue and runs each turn body serially, awaited to
//! completion before the next — this is the FIFO guarantee (fixes the mutex
//! race where two same-session tasks could win the free lock out of order).
//!
//! Lifecycle: consumers exit on `recv() == None` (all senders dropped). There
//! is NO active idle-eviction — entries live for the connection's lifetime and
//! die when the per-connection map is dropped at teardown. This removes the
//! check-and-remove TOCTOU the mutex map needed `remove_if` for. The consumer
//! captures ONLY its `Receiver`, never `Arc<SessionQueueMap>`, so a dropped map
//! actually drops every sender.

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use opex_types::{ChannelOutbound, IncomingMessageDto};

use super::types::{InflightMessage, InflightRegistry, OutboundMsg, SessionKey};
use crate::agent::engine::{AgentEngine, ProcessingPhase};

/// Everything a queued turn needs to run without borrowing reader state.
pub(super) struct QueuedTurn {
    pub engine: Arc<AgentEngine>,
    pub agent_name: String,
    pub channel_type: String,
    pub formatting_prompt: Option<String>,
    pub request_id: String,
    pub msg: IncomingMessageDto,
    pub timeout_secs: u64,
    pub out_tx: mpsc::Sender<OutboundMsg>,
    pub inflight: InflightRegistry,
    pub cancel_token: tokio_util::sync::CancellationToken,
}

#[derive(Default)]
pub(super) struct SessionQueueMap {
    inner: DashMap<SessionKey, mpsc::UnboundedSender<QueuedTurn>>,
}

impl SessionQueueMap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: DashMap::new() })
    }

    /// Enqueue a turn for its session, in caller (receive) order. Get-or-create
    /// the per-key sender, spawning a consumer on first use. If the existing
    /// sender's consumer has died (panicked/exited — `send` returns `Err`),
    /// evict the stale entry, respawn a consumer, and resend. Never blocks: the
    /// unbounded send is synchronous.
    pub async fn enqueue(self: &Arc<Self>, key: SessionKey, turn: QueuedTurn) {
        // Fast path: existing live sender.
        if let Some(sender) = self.inner.get(&key) {
            if let Err(mpsc::error::SendError(returned)) = sender.send(turn) {
                // Consumer gone — fall through to respawn with the returned turn.
                drop(sender);
                self.respawn_and_send(key, returned);
            }
            return;
        }
        // Slow path: create sender + consumer.
        self.respawn_and_send(key, turn);
    }

    /// Create a fresh sender+consumer for `key` (replacing any stale entry) and
    /// send `turn`. The consumer captures ONLY `rx` — never `self`.
    fn respawn_and_send(self: &Arc<Self>, key: SessionKey, turn: QueuedTurn) {
        let (tx, rx) = mpsc::unbounded_channel::<QueuedTurn>();
        // Insert BEFORE sending so a concurrent enqueue for the same key finds
        // the live sender. `insert` overwrites any stale (dead-consumer) entry.
        self.inner.insert(key, tx.clone());
        tokio::spawn(consumer(rx));
        // The consumer is alive; this send cannot fail.
        let _ = tx.send(turn);
    }

    #[cfg(test)]
    pub fn entry_count(&self) -> usize {
        self.inner.len()
    }
}

/// Drain one session's queue serially. Runs each turn body to completion before
/// the next (FIFO). Exits when all senders drop (`recv() == None`).
async fn consumer(mut rx: mpsc::UnboundedReceiver<QueuedTurn>) {
    while let Some(turn) = rx.recv().await {
        let request_id = turn.request_id.clone();
        let inflight = turn.inflight.clone();

        // Cancelled while queued: the reader's Cancel arm already emitted the
        // "Cancelled" frame (dispatcher::cancel returned true for the registered
        // request_id). Just drop the entry and skip — do NOT re-run or re-emit.
        if turn.cancel_token.is_cancelled() {
            inflight.lock().await.remove(&request_id);
            continue;
        }

        // Spawn the turn body so a sync-wedged turn can be hard-aborted via its
        // AbortHandle without killing this consumer. Await it → strict FIFO.
        let handle = tokio::spawn(run_turn_body(turn));
        if let Some(im) = inflight.lock().await.get_mut(&request_id) {
            im.abort = Some(handle.abort_handle());
        }
        let _ = handle.await;
        inflight.lock().await.remove(&request_id);
    }
}

/// Run a single turn to completion: forward chunks/phases, run the engine with
/// the request timeout + cooperative-cancel grace, emit the final frame. Moved
/// verbatim from the old `dispatcher::dispatch_message` spawned body, minus the
/// session-lock acquire and the inflight self-removal (the consumer owns both).
async fn run_turn_body(turn: QueuedTurn) {
    let QueuedTurn {
        engine, agent_name, channel_type, formatting_prompt,
        request_id, msg, timeout_secs, out_tx, inflight: _, cancel_token,
    } = turn;

    let incoming = msg.into_incoming(
        engine.cfg().agent.name.clone(),
        channel_type.clone(),
        formatting_prompt,
    );

    let (status_tx, mut status_rx) = mpsc::unbounded_channel::<ProcessingPhase>();
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<String>(512);

    let chunk_out = out_tx.clone();
    let chunk_req = request_id.clone();
    let chunk_forwarder = tokio::spawn(async move {
        while let Some(text) = chunk_rx.recv().await {
            let m = ChannelOutbound::Chunk { request_id: chunk_req.clone(), text };
            if chunk_out.send(OutboundMsg::Wire(m)).await.is_err() { return; }
        }
    });

    let phase_out = out_tx.clone();
    let phase_req = request_id.clone();
    let phase_forwarder = tokio::spawn(async move {
        while let Some(phase) = status_rx.recv().await {
            let (p, t) = phase.to_wire();
            let m = ChannelOutbound::Phase { request_id: phase_req.clone(), phase: p, tool_name: t };
            if phase_out.send(OutboundMsg::Wire(m)).await.is_err() { return; }
        }
    });

    let engine_fut = engine.handle_with_status(
        &incoming, Some(status_tx), Some(chunk_tx), cancel_token.clone(),
    );
    let result = if timeout_secs > 0 {
        let mut fut = Box::pin(engine_fut);
        let dur = std::time::Duration::from_secs(timeout_secs);
        match tokio::time::timeout(dur, &mut fut).await {
            Ok(r) => r,
            Err(_) => {
                cancel_token.cancel();
                const TIMEOUT_GRACE: std::time::Duration = std::time::Duration::from_secs(20);
                match tokio::time::timeout(TIMEOUT_GRACE, &mut fut).await {
                    Ok(r) => r,
                    Err(_) => {
                        drop(fut);
                        Err(anyhow::anyhow!(
                            "Request timed out after {timeout_secs}s. The task was too complex or an external service was slow.",
                        ))
                    }
                }
            }
        }
    } else {
        engine_fut.await
    };

    let _ = chunk_forwarder.await;
    let _ = phase_forwarder.await;

    let final_msg = match result {
        Ok(text) => ChannelOutbound::Done { request_id: request_id.clone(), text },
        Err(e) => ChannelOutbound::Error { request_id: request_id.clone(), message: e.to_string() },
    };
    if out_tx.send(OutboundMsg::Wire(final_msg)).await.is_err() {
        tracing::debug!(agent = %agent_name, %request_id, "out_tx closed before final frame");
    }
}

/// Stop the in-flight (queued OR running) task for `request_id` COOPERATIVELY.
/// Returns true if an entry existed (the reader emits the user-visible frame).
///
/// R-CHANNEL: cancels the turn's token so a running turn reaches finalize
/// ('interrupted'); a queued turn's consumer observes `is_cancelled()` and
/// skips it. The optional `abort` is a post-grace backstop for a sync-wedged
/// running turn only — a queued turn has `abort = None` and needs no backstop.
pub(super) async fn cancel(request_id: &str, inflight: &InflightRegistry) -> bool {
    if let Some(entry) = inflight.lock().await.remove(request_id) {
        entry.cancel.cancel();
        if let Some(abort) = entry.abort {
            tokio::spawn(async move {
                const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(20);
                tokio::time::sleep(CANCEL_GRACE).await;
                abort.abort();
            });
        }
        true
    } else {
        false
    }
}
```

- [ ] **Step 5: Write unit tests in `session_queue.rs`**

These use a stub turn that does not need a real engine. To test FIFO/respawn/cancel without `AgentEngine`, test the queue plumbing via a minimal helper: send `QueuedTurn`s whose observable effect is a recorded order. Since `run_turn_body` needs a real engine, the tests target the queue's ordering + respawn + cancel-skip using a test-only consumer hook. Add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    fn key(user: &str) -> SessionKey {
        SessionKey {
            agent_name: "Arty".to_string(),
            eff_user: user.to_string(),
            eff_channel: "telegram".to_string(),
            eff_chat_scope: None,
        }
    }

    fn inflight() -> InflightRegistry {
        Arc::new(Mutex::new(std::collections::HashMap::new()))
    }

    /// FIFO: a single unbounded receiver drains in send order. We assert the
    /// ordering property of the queue transport directly (the consumer awaits
    /// each turn, so send-order == process-order).
    #[tokio::test]
    async fn unbounded_queue_preserves_send_order() {
        let (tx, mut rx) = mpsc::unbounded_channel::<usize>();
        for i in 0..10 { tx.send(i).unwrap(); }
        drop(tx);
        let mut seen = vec![];
        while let Some(v) = rx.recv().await { seen.push(v); }
        assert_eq!(seen, (0..10).collect::<Vec<_>>(), "queue must drain in send order");
    }

    /// Respawn signal: `enqueue`'s dead-consumer detection relies on a sender
    /// reporting `is_closed()` once its receiver is dropped. Assert that signal
    /// (the full run_turn_body respawn path needs a live engine → covered by
    /// the server test session + E2E, not this unit test).
    #[tokio::test]
    async fn dropped_receiver_closes_sender() {
        let (tx, rx) = mpsc::unbounded_channel::<QueuedTurn>();
        assert!(!tx.is_closed(), "sender live while receiver exists");
        drop(rx); // consumer "died"
        assert!(tx.is_closed(), "sender must report closed once its receiver dropped — enqueue respawns on this");
    }

    /// Cancel-of-queued: an entry registered with a cancelled token is skipped
    /// by the consumer's `is_cancelled()` guard (asserted at the guard level).
    #[tokio::test]
    async fn cancelled_token_is_observed() {
        use tokio_util::sync::CancellationToken;
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled(), "consumer skip-guard reads is_cancelled()");
    }

    /// cancel() removes the entry, cancels the token, returns true; unknown → false.
    #[tokio::test]
    async fn cancel_signals_and_removes() {
        use tokio_util::sync::CancellationToken;
        let reg = inflight();
        let token = CancellationToken::new();
        reg.lock().await.insert(
            "r1".to_string(),
            InflightMessage { cancel: token.clone(), abort: None },
        );
        assert!(cancel("r1", &reg).await);
        assert!(reg.lock().await.is_empty());
        assert!(token.is_cancelled());
        assert!(!cancel("never", &reg).await);
    }
}
```

Note on the respawn test: `mpsc::UnboundedSender` has no `send_timeout_marker`; replace that assertion line with the real check — `assert!(stale.is_closed(), "stale sender closed once receiver dropped");` and delete the `||` clause. (Written this way so the implementer verifies the closed-sender signal the respawn path relies on.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p opex-core --bin opex-core session_queue -- --nocapture`
Expected: PASS (4 tests). If `is_closed()` is the correct API (it is for tokio `UnboundedSender`), the respawn test compiles.

- [ ] **Step 7: Local check + clippy**

Run: `cargo check -p opex-core --bin opex-core` then `cargo clippy -p opex-core --bin opex-core -- -D warnings`
Expected: clean. (Reader/mod still reference the old modules → they are fixed in Tasks 3-4; this task's `cargo check` will FAIL to compile the crate until Tasks 3-4 land. Run `cargo check` scoped so: expect errors ONLY in `reader.rs`/`mod.rs` referencing `dispatcher`/`session_locks`. Those are resolved in Tasks 3-4. Commit this task despite the crate-level break — Tasks 3-4 complete the wiring. The reviewer is told the crate compiles only after Task 4.)

- [ ] **Step 8: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/session_queue.rs \
        crates/opex-core/src/gateway/handlers/channel_ws/types.rs \
        crates/opex-core/src/gateway/handlers/channel_ws/mod.rs
git add -u  # stage the two deletions
git commit -m "feat(channel-ws): SessionQueueMap + QueuedTurn + moved turn body (#1) [wiring in follow-up tasks]"
```

---

### Task 2: Sync `*_matches` classifiers in `inline.rs`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` (four callback handlers: approval ~171, initiative ~301, infra ~458, clarify-cb ~553)
- Test: inline `#[cfg(test)]` module in `inline.rs`

**Interfaces:**
- Produces four sync classifiers, each returning whether the message is that callback family (`is_callback && known-prefix`), plus a small helper:
  - `pub(super) fn is_callback(msg: &IncomingMessageDto) -> bool`
  - `pub(super) fn approval_matches(msg: &IncomingMessageDto) -> bool`
  - `pub(super) fn initiative_matches(msg: &IncomingMessageDto) -> bool`
  - `pub(super) fn infra_matches(msg: &IncomingMessageDto) -> bool`
  - `pub(super) fn clarify_cb_matches(msg: &IncomingMessageDto) -> bool`
- Each handler's inline `is_callback`+prefix preamble is refactored to `if !X_matches(msg) { return false; }` so the reader's classifier (Task 3) and the handler share ONE prefix definition — zero drift.

**Background:** The reader must decide, synchronously and before spawning, which handler (if any) a `Message` belongs to — spawning is fire-and-forget, so the old "call handler, branch on its bool return" no longer works. The prefixes: approval `approve:`/`reject:`; initiative `iappr:`/`idismiss:`/`icancel:`/`dpm:approve:`/`dpm:dismiss:`; infra `infra:ok:`/`infra:no:`; clarify-cb `clarify:{id}:{slot}` (via `parse_clarify_callback`). All gate on `is_callback == true` first.

- [ ] **Step 1: Write the failing tests**

Add a test module in `inline.rs`:

```rust
#[cfg(test)]
mod classifier_tests {
    use super::*;
    use opex_types::IncomingMessageDto;

    fn dto(text: &str, is_cb: bool) -> IncomingMessageDto {
        let mut d = IncomingMessageDto::default();
        d.text = Some(text.to_string());
        d.context = serde_json::json!({ "is_callback": is_cb });
        d
    }

    #[test]
    fn approval_matches_only_with_callback_flag() {
        assert!(approval_matches(&dto("approve:abc", true)));
        assert!(approval_matches(&dto("reject:abc", true)));
        assert!(!approval_matches(&dto("approve:abc", false)), "plain text lookalike is NOT a callback");
        assert!(!approval_matches(&dto("hello", true)));
    }

    #[test]
    fn initiative_matches_all_prefixes() {
        for p in ["iappr:x", "idismiss:x", "icancel:x", "dpm:approve:x", "dpm:dismiss:x"] {
            assert!(initiative_matches(&dto(p, true)), "{p} must match");
        }
        assert!(!initiative_matches(&dto("iappr:x", false)));
        assert!(!initiative_matches(&dto("infra:ok:x", true)));
    }

    #[test]
    fn infra_matches_prefixes() {
        assert!(infra_matches(&dto("infra:ok:x", true)));
        assert!(infra_matches(&dto("infra:no:x", true)));
        assert!(!infra_matches(&dto("infra:ok:x", false)));
        assert!(!infra_matches(&dto("infra:maybe:x", true)));
    }

    #[test]
    fn clarify_cb_matches_valid_form() {
        assert!(clarify_cb_matches(&dto("clarify:11111111-1111-1111-1111-111111111111:0", true)));
        assert!(!clarify_cb_matches(&dto("clarify:bad", true)), "malformed clarify is not a clarify callback");
        assert!(!clarify_cb_matches(&dto("clarify:11111111-1111-1111-1111-111111111111:0", false)));
    }

    #[test]
    fn plain_text_that_looks_like_prefix_is_not_a_callback() {
        // The design-review "message vanishes" case: user types "infra:ok: yes".
        let m = dto("infra:ok: yes", false);
        assert!(!approval_matches(&m) && !initiative_matches(&m) && !infra_matches(&m) && !clarify_cb_matches(&m),
            "no classifier may claim a plain-text message");
    }
}
```

(If `IncomingMessageDto` does not derive `Default`, construct it explicitly in `dto()` with the minimal required fields — the implementer adapts to the actual struct; the assertions are the contract.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p opex-core --bin opex-core classifier_tests -- --nocapture`
Expected: FAIL to compile — classifiers not defined.

- [ ] **Step 3: Add the classifiers**

Add near `parse_clarify_callback` in `inline.rs`:

```rust
/// True iff the adapter tagged this message as an inline-button callback.
pub(super) fn is_callback(msg: &IncomingMessageDto) -> bool {
    msg.context
        .get("is_callback")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Sync classifier: is this an approval callback (`approve:`/`reject:`)?
pub(super) fn approval_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    let t = msg.text.as_deref().unwrap_or("");
    t.starts_with("approve:") || t.starts_with("reject:")
}

/// Sync classifier: is this an initiative callback?
pub(super) fn initiative_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    let t = msg.text.as_deref().unwrap_or("");
    t.starts_with("iappr:") || t.starts_with("idismiss:") || t.starts_with("icancel:")
        || t.starts_with("dpm:approve:") || t.starts_with("dpm:dismiss:")
}

/// Sync classifier: is this an infra-decision callback?
pub(super) fn infra_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    let t = msg.text.as_deref().unwrap_or("");
    t.starts_with("infra:ok:") || t.starts_with("infra:no:")
}

/// Sync classifier: is this a clarify button callback (`clarify:{id}:{slot}`)?
pub(super) fn clarify_cb_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    parse_clarify_callback(msg.text.as_deref().unwrap_or("")).is_some()
}
```

- [ ] **Step 4: Refactor each handler preamble to call its classifier**

In `handle_approval_callback`, replace the `is_callback` + prefix block (lines 179-195) with:

```rust
    if !approval_matches(msg) {
        return false;
    }
    let text = msg.text.as_deref().unwrap_or("");
    let approval_id_str = text
        .strip_prefix("approve:")
        .or_else(|| text.strip_prefix("reject:"))
        .expect("approval_matches guaranteed a known prefix");
    let approved = text.starts_with("approve:");
    let user_id = msg.user_id.clone();
```

In `handle_initiative_callback`, replace lines 309-322 with `if !initiative_matches(msg) { return false; }` followed by the existing `let user_id = msg.user_id.clone();`.

In `handle_infra_callback`, replace lines 466-473 with `if !infra_matches(msg) { return false; }`, keeping the existing `let (rest, approved) = ...` strip block (476-482) as-is (it re-derives which prefix matched).

In `handle_clarify_callback`, replace lines 561-573 with:

```rust
    if !clarify_cb_matches(msg) {
        return false;
    }
    let text = msg.text.as_deref().unwrap_or("");
    let (clarify_id, slot) = parse_clarify_callback(text)
        .expect("clarify_cb_matches guaranteed a valid clarify callback");
    let user_id = msg.user_id.clone();
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p opex-core --bin opex-core classifier_tests -- --nocapture`
Expected: PASS (5 tests). (Crate still won't fully compile until Tasks 3-4 — scope the check to accept only reader/mod errors.)

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/inline.rs
git commit -m "refactor(channel-ws): extract sync callback classifiers (shared with reader) (#3)"
```

---

### Task 3: Reader — sync classify → spawn / enqueue

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` (imports; `run` signature `lock_map` → `queue_map`; Message arm 101-167; Cancel arm 168-183; `wire_guards` tests 238-278)
- Test: same file

**Interfaces:**
- Consumes: `session_queue::{SessionQueueMap, QueuedTurn, enqueue, cancel}`, `inline::{is_callback, approval_matches, initiative_matches, infra_matches, clarify_cb_matches, handle_*_callback, handle_clarify_text}`, `types::{InflightMessage, SessionKey}`.
- Produces: `reader::run(..., queue_map: Arc<SessionQueueMap>, ...)` (was `lock_map: Arc<SessionLockMap>`). Behaviour: Message arm classifies sync, spawns callbacks / spawns clarify-text resolver / registers inflight + enqueues turn.

**Background:** Today the Message arm calls five interceptors inline (each awaiting engine/DB), then `dispatch_message`. The rewrite classifies synchronously and never awaits engine/DB on the hot path. The #6 handshake guard from Batch A stays as the first statement. Clarify-text is resolved in a spawned short-lived task ONLY when `has_any_pending()` (sync) is true — off both the reader and the FIFO consumer (deadlock-safe).

- [ ] **Step 1: Update the failing `wire_guards` tests**

The old textual-order guards assert interceptors precede `dispatch_message` in source. That call is gone (now `enqueue`). Replace the whole `wire_guards` module (238-278) with tests that assert the new routing structure:

```rust
#[cfg(test)]
mod wire_guards {
    // Structural guards on the reader's Message-arm routing. These assert the
    // sync-classify order (approval > initiative > infra > clarify-cb >
    // clarify-text > enqueue) is preserved in source, matching the priority the
    // inline handlers enforced by call order before the queue rewrite.

    #[test]
    fn handshake_guard_before_routing() {
        let src = include_str!("reader.rs");
        let guard = src.find("channel_type == \"unknown\"").expect("#6 handshake guard present");
        let route = src.find("approval_matches(").expect("classifier routing present");
        assert!(guard < route, "handshake guard must run before routing");
    }

    #[test]
    fn approval_classified_before_clarify_text() {
        let src = include_str!("reader.rs");
        let approval = src.find("approval_matches(").expect("approval classifier present");
        let clarify = src.find("handle_clarify_text(").expect("clarify-text spawn present");
        assert!(approval < clarify, "approval must be classified before clarify-text (priority)");
    }

    #[test]
    fn callbacks_classified_before_enqueue() {
        let src = include_str!("reader.rs");
        let clarify_cb = src.find("clarify_cb_matches(").expect("clarify-cb classifier present");
        let enqueue = src.find("queue_map.enqueue(").expect("turn enqueue present");
        assert!(clarify_cb < enqueue, "all callback classifiers must precede enqueue");
    }

    #[test]
    fn clarify_text_gated_by_has_any_pending() {
        let src = include_str!("reader.rs");
        assert!(src.contains("has_any_pending()"), "clarify-text spawn must be gated by the sync has_any_pending() check");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p opex-core --bin opex-core wire_guards -- --nocapture`
Expected: FAIL — new symbols not present in source yet.

- [ ] **Step 3: Update imports + signature**

In `reader.rs`, replace `use super::session_locks::SessionLockMap;` with `use super::session_queue::{self, QueuedTurn, SessionQueueMap};` and drop the `use super::{dispatcher, handshake, inline};` reference to `dispatcher` → `use super::{handshake, inline};`.

Change the `run` parameter `lock_map: Arc<SessionLockMap>` → `queue_map: Arc<SessionQueueMap>`.

- [ ] **Step 4: Rewrite the Message arm**

Replace the entire `ChannelInbound::Message { request_id, msg }` arm body (lines 102-167, i.e. everything after the Batch-A #6 guard through the `session_updated` UI event) with:

```rust
                        // Bump last_activity for stale-channel detection.
                        {
                            let mut chans = ctx.bus.connected_channels.write().await;
                            if let Some(c) = chans
                                .iter_mut()
                                .find(|c| c.agent_name == agent_name && c.channel_type == state.channel_type)
                            {
                                c.last_activity = chrono::Utc::now();
                            }
                        }
                        ctx.status.polling_diagnostics.record_inbound();

                        // Sync classification (never awaits engine/DB). Priority:
                        // approval > initiative > infra > clarify-cb > clarify-text
                        // > turn. Callbacks are spawned off the hot path; the one
                        // async-to-classify path (clarify-text) is gated by the
                        // sync has_any_pending() and spawned — NOT run in the FIFO
                        // consumer (deadlock-safe).
                        if inline::approval_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_approval_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if inline::initiative_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_initiative_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if inline::infra_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_infra_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if inline::clarify_cb_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_clarify_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if !inline::is_callback(&msg)
                            && engine.cfg().clarify_manager.has_any_pending()
                        {
                            // Clarify-text: a clarify is pending. Spawn a short-lived
                            // resolver (async DB lookup + owner-gate). If it does NOT
                            // resolve a waiter, it enqueues the message as a turn.
                            let ct = state.channel_type.clone();
                            let fp = state.formatting_prompt.clone();
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            let qmap = queue_map.clone();
                            let inflight2 = inflight.clone();
                            let timeout = ctx.cfg.config.limits.request_timeout_secs;
                            tokio::spawn(async move {
                                let consumed = inline::handle_clarify_text(
                                    &ctx2, &engine2, &an, &ct, &rid, &m, &tx,
                                ).await;
                                if !consumed {
                                    enqueue_turn(
                                        &qmap, &engine2, &an, &ct, fp,
                                        rid, m, timeout, &tx, &inflight2,
                                    ).await;
                                }
                            });
                        } else {
                            // Ordinary turn — register inflight at enqueue time then
                            // enqueue in receive order.
                            enqueue_turn(
                                &queue_map, &engine, &agent_name, &state.channel_type,
                                state.formatting_prompt.clone(),
                                request_id, msg, ctx.cfg.config.limits.request_timeout_secs,
                                &out_tx, &inflight,
                            ).await;
                        }

                        // UI sidebar refresh (preserved from old loop).
                        let event = serde_json::json!({
                            "type": "session_updated",
                            "agent": agent_name,
                            "channel": state.channel_type,
                        });
                        ctx.bus.ui_event_tx.send(event.to_string()).ok();
```

Note: the clarify-text spawn passes `state.formatting_prompt` via a helper because `state` is not `Send`-movable into the task. Simplest: capture `state.formatting_prompt.clone()` into a local BEFORE the spawn and move it in, instead of `state_prompt(&engine2)`. Replace `state_prompt(&engine2)` with a pre-captured `let fp = state.formatting_prompt.clone();` moved into the task. (The implementer wires the captured `fp`; `state_prompt` is a placeholder to be removed.)

- [ ] **Step 5: Add the `enqueue_turn` helper**

Add a private helper in `reader.rs` (above `run` or below it) that computes the `SessionKey`, registers inflight at enqueue, and enqueues:

```rust
/// Register the turn in `inflight` (cancel token, `abort = None`) and enqueue it
/// for its session in receive order. Registering at ENQUEUE (not consumer start)
/// is what lets a `Cancel` for a still-queued turn be honoured.
#[allow(clippy::too_many_arguments)]
async fn enqueue_turn(
    queue_map: &Arc<SessionQueueMap>,
    engine: &Arc<AgentEngine>,
    agent_name: &str,
    channel_type: &str,
    formatting_prompt: Option<String>,
    request_id: String,
    msg: opex_types::IncomingMessageDto,
    timeout_secs: u64,
    out_tx: &mpsc::Sender<OutboundMsg>,
    inflight: &InflightRegistry,
) {
    let dm_scope = engine
        .cfg()
        .agent
        .session
        .as_ref()
        .map(|s| s.dm_scope.as_str())
        .unwrap_or("per-channel-peer")
        .to_string();
    let chat_scope = msg.chat_scope();
    let session_key = SessionKey::from_inbound(
        agent_name, &msg.user_id, channel_type, &dm_scope, chat_scope.as_deref(),
    );

    let cancel_token = tokio_util::sync::CancellationToken::new();
    inflight.lock().await.insert(
        request_id.clone(),
        InflightMessage { cancel: cancel_token.clone(), abort: None },
    );

    let turn = QueuedTurn {
        engine: engine.clone(),
        agent_name: agent_name.to_string(),
        channel_type: channel_type.to_string(),
        formatting_prompt,
        request_id,
        msg,
        timeout_secs,
        out_tx: out_tx.clone(),
        inflight: inflight.clone(),
        cancel_token,
    };
    queue_map.enqueue(session_key, turn).await;
}
```

Add the needed imports to `reader.rs`: `use super::types::{InflightMessage, SessionKey};` (extend the existing `types` import) and ensure `InflightRegistry`, `OutboundMsg` are already imported (they are).

- [ ] **Step 6: Point the Cancel arm at `session_queue::cancel`**

In the `ChannelInbound::Cancel` arm (168-183), replace `dispatcher::cancel(&request_id, &inflight)` with `session_queue::cancel(&request_id, &inflight)`. The rest of the arm (emit "Cancelled" on true) is unchanged.

- [ ] **Step 7: Run the wire_guards tests**

Run: `cargo test -p opex-core --bin opex-core wire_guards -- --nocapture`
Expected: PASS (4 tests).

- [ ] **Step 8: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/reader.rs
git commit -m "feat(channel-ws): reader sync-classify → spawn callbacks / spawn clarify-text / enqueue turn (#1,#3)"
```

---

### Task 4: mod.rs — construct queue map + teardown

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/mod.rs` (`channel_ws_loop`: construct map ~141, pass to reader ~269, teardown 287-315)
- Test: crate build + existing suite

**Interfaces:**
- Consumes: `session_queue::SessionQueueMap`.
- Produces: `channel_ws_loop` constructs `SessionQueueMap`, hands it to the reader, and on teardown cancels every inflight token (running turns interrupt cooperatively) with a bounded grace + post-grace abort of any attached `abort` handle.

**Background:** The mutex map was moved into the reader and dropped on return; teardown then drained per-turn `JoinHandle`s. In the queue model there are no per-turn join handles — the map (and its senders) drop when the reader returns, so consumers drain their queues and exit on `recv()==None`. Teardown's remaining job is to cancel in-flight tokens so a running turn reaches finalize ('interrupted') promptly, and to hard-abort a sync-wedged turn after a grace via its stored `AbortHandle`.

- [ ] **Step 1: Construct the queue map**

In `channel_ws_loop`, replace line 141 `let lock_map = session_locks::SessionLockMap::new();` with:

```rust
    let queue_map = session_queue::SessionQueueMap::new();
```

- [ ] **Step 2: Pass it to the reader**

In the `reader::run(...)` call (263-275), replace the `lock_map,` argument with `queue_map,`. (The reader now owns the only `Arc<SessionQueueMap>`; on return it drops, senders drop, consumers wind down.)

- [ ] **Step 3: Rewrite teardown to cancel + grace + abort**

Replace the teardown block (287-315) with:

```rust
    // Tear down in-flight turns COOPERATIVELY (R-CHANNEL). The queue map was
    // dropped when the reader returned, so consumers are already draining to
    // exit; we only need to interrupt RUNNING turns promptly (cancel token →
    // execute() returns Interrupted → finalize marks 'interrupted') and, after a
    // bounded grace, hard-abort any sync-wedged turn via its stored AbortHandle.
    {
        let drained: Vec<_> = {
            let mut g = inflight.lock().await;
            g.drain().map(|(_, im)| im).collect()
        };
        if !drained.is_empty() {
            for im in &drained {
                im.cancel.cancel();
            }
            let grace = std::time::Duration::from_secs(15);
            tokio::time::sleep(grace).await;
            for im in &drained {
                if let Some(abort) = &im.abort {
                    abort.abort();
                }
            }
        }
    }
```

- [ ] **Step 4: Full crate build + clippy (this is the first point the crate compiles)**

Run: `cargo check -p opex-core --bin opex-core`
Expected: clean — all references to `dispatcher`/`session_locks`/`lock_map`/old `InflightMessage` are gone.
Run: `cargo clippy -p opex-core --bin opex-core -- -D warnings`
Expected: 0 warnings.

- [ ] **Step 5: Run the full channel_ws test set**

Run: `cargo test -p opex-core --bin opex-core -- channel_ws session_queue classifier_tests wire_guards writer ready_guard --nocapture`
Expected: PASS. (Local Windows runs may be unreliable per Global Constraints — the authoritative run is the server session.)

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/mod.rs
git commit -m "feat(channel-ws): construct SessionQueueMap; teardown cancels inflight + abort backstop (#1)"
```

---

## Known behaviour changes (call out in whole-branch review)

- **Sync-wedged running turn on /stop:** previously the per-turn `JoinHandle` was hard-aborted 20s after Cancel. Now a cancelled sync-wedged turn is aborted via its `AbortHandle` 20s after Cancel (`session_queue::cancel`), so parity holds for running turns. A turn cancelled while still QUEUED is simply skipped (no work started) — strictly better.
- **No active idle-eviction:** queue entries live for the connection lifetime (one idle sender + parked consumer per distinct SessionKey seen). Bounded by connection scope; documented tradeoff for removing the TOCTOU class.
- **Clarify-text ordering edge:** while a clarify is pending, a non-answer plain message may be enqueued after a short async hop (resolver spawn) rather than strictly inline. Matches today's out-of-band clarify resolution; only occurs in the narrow pending-clarify window.
- **Cross-adapter serialization** remains unenforced (per-connection map) — pre-existing, spec §9.

## Post-implementation (controller, after whole-branch review + user approval)

- Server test session in `~/opex-src` (throttled): `CARGO_BUILD_JOBS=4 nice ionice cargo test -p opex-core --bin opex-core -- channel_ws session_queue classifier_tests wire_guards writer`.
- `make remote-deploy` on explicit user approval.
- E2E smoke: (1) two rapid same-session messages → replies in order; (2) an approval inline-button still resolves (spawned callback); (3) a plain message "infra:ok: test" (NOT a callback) is answered as a normal turn, not swallowed; (4) mid-run clarify: agent asks, user answers via text → resolves without the deadlock; (5) /stop a queued second turn → skipped.
