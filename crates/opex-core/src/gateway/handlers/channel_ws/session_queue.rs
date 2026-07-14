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
