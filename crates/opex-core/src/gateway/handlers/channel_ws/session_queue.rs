//! Per-session FIFO turn queue. Replaces the per-`SessionKey` mutex.
//!
//! A `SessionQueueMap` holds one `mpsc::UnboundedSender<QueuedTurn>` per active
//! `SessionKey`. The reader enqueues turns in receive order; a lazily-spawned
//! consumer task drains its queue and runs each turn body serially, awaited to
//! completion before the next — this is the FIFO guarantee (fixes the mutex
//! race where two same-session tasks could win the free lock out of order).
//! This strict per-session FIFO holds for turns enqueued directly by the
//! reader; when a clarify is pending, two plain-text messages routed through
//! spawned clarify-text resolvers may enqueue out of receive order (inherent
//! to spawning them off the FIFO consumer for deadlock-safety).
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

use super::types::{InflightRegistry, OutboundMsg, SessionKey};
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

    /// Enqueue a turn for its session, in caller (receive) order. Atomically
    /// gets-or-creates the per-key sender+consumer via DashMap's `entry` API
    /// (the shard write lock is held across the synchronous `tokio::spawn`, so
    /// exactly ONE consumer is ever created per key — no double-consumer race).
    /// Panic-safe: if the consumer has died (sender closed), evict and retry,
    /// which re-creates a fresh consumer on the next `or_insert_with`.
    pub async fn enqueue(self: &Arc<Self>, key: SessionKey, mut turn: QueuedTurn) {
        loop {
            let sender = self
                .inner
                .entry(key.clone())
                .or_insert_with(|| {
                    let (tx, rx) = mpsc::unbounded_channel::<QueuedTurn>();
                    tokio::spawn(consumer(rx, run_turn_body));
                    tx
                })
                .clone();
            match sender.send(turn) {
                Ok(()) => return,
                Err(mpsc::error::SendError(returned)) => {
                    // Consumer died (receiver dropped) → evict the closed sender
                    // so the next iteration's or_insert_with makes a fresh one.
                    self.inner.remove_if(&key, |_, v| v.is_closed());
                    turn = returned;
                }
            }
        }
    }
}

/// The per-turn bookkeeping the queue consumer needs, abstracted so the
/// consumer loop can be unit-tested with a lightweight fake turn (no engine).
pub(super) trait QueuedItem: Send + 'static {
    fn request_id(&self) -> &str;
    fn inflight(&self) -> &InflightRegistry;
    /// The turn's cancel token clone (same token registered in `inflight`).
    /// Only exercised by tests, which simulate a `Cancel` racing ahead of the
    /// consumer by cancelling the token before the turn is sent.
    #[cfg(test)]
    fn cancel_token(&self) -> &tokio_util::sync::CancellationToken;
}

impl QueuedItem for QueuedTurn {
    fn request_id(&self) -> &str {
        &self.request_id
    }
    fn inflight(&self) -> &InflightRegistry {
        &self.inflight
    }
    #[cfg(test)]
    fn cancel_token(&self) -> &tokio_util::sync::CancellationToken {
        &self.cancel_token
    }
}

/// Drain one session's queue serially. Runs each turn body to completion before
/// the next (FIFO). Exits when all senders drop (`recv() == None`).
async fn consumer<T, R, Fut>(mut rx: mpsc::UnboundedReceiver<T>, run: R)
where
    T: QueuedItem,
    R: Fn(T) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    while let Some(turn) = rx.recv().await {
        let request_id = turn.request_id().to_string();
        let inflight = turn.inflight().clone();

        // Atomically decide skip-or-run and attach the abort handle under ONE
        // lock acquisition, so a concurrent cancel() cannot remove an
        // abort:None entry between the spawn and the attach (which would drop
        // the sync-wedge hard-abort backstop).
        let handle = {
            let mut guard = inflight.lock().await;
            match guard.get_mut(&request_id) {
                // cancel() removed the entry while it was queued → skip (the
                // reader's Cancel arm already emitted the "Cancelled" frame).
                None => continue,
                // token cancelled while queued → skip, drop the entry.
                Some(im) if im.cancel.is_cancelled() => {
                    guard.remove(&request_id);
                    continue;
                }
                Some(im) => {
                    let h = tokio::spawn(run(turn));
                    im.abort = Some(h.abort_handle());
                    h
                }
            }
        };
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
    use super::super::types::InflightMessage;
    use tokio::sync::Mutex;

    fn inflight() -> InflightRegistry {
        Arc::new(Mutex::new(std::collections::HashMap::new()))
    }

    /// Lightweight fake turn so the generic `consumer` can be driven without an
    /// `AgentEngine`.
    struct TestTurn {
        request_id: String,
        inflight: InflightRegistry,
        cancel: tokio_util::sync::CancellationToken,
    }
    impl QueuedItem for TestTurn {
        fn request_id(&self) -> &str {
            &self.request_id
        }
        fn inflight(&self) -> &InflightRegistry {
            &self.inflight
        }
        fn cancel_token(&self) -> &tokio_util::sync::CancellationToken {
            &self.cancel
        }
    }

    /// Register an inflight entry for `rid` (as the reader would at enqueue)
    /// and build a matching `TestTurn`.
    fn make_turn(reg: &InflightRegistry, rid: &str) -> TestTurn {
        let cancel = tokio_util::sync::CancellationToken::new();
        // Best-effort synchronous insert: tests run on the single-threaded
        // current_thread flavor by default, but `blocking_lock` would panic in
        // an async context, so use `try_lock` (registry is uncontended here).
        reg.try_lock()
            .expect("registry uncontended in test setup")
            .insert(
                rid.to_string(),
                InflightMessage { cancel: cancel.clone(), abort: None },
            );
        TestTurn { request_id: rid.to_string(), inflight: reg.clone(), cancel }
    }

    /// FIFO + exactly-once: turns are processed in send order and each
    /// inflight entry is removed once its turn completes.
    #[tokio::test]
    async fn consumer_processes_turns_in_fifo_order() {
        let reg = inflight();
        let (tx, rx) = mpsc::unbounded_channel::<TestTurn>();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let recorder = seen.clone();
        let handle = tokio::spawn(consumer(rx, move |turn: TestTurn| {
            let recorder = recorder.clone();
            async move {
                recorder.lock().await.push(turn.request_id().to_string());
            }
        }));

        for rid in ["a", "b", "c"] {
            let turn = make_turn(&reg, rid);
            tx.send(turn).unwrap();
        }
        drop(tx);
        handle.await.unwrap();

        assert_eq!(*seen.lock().await, vec!["a", "b", "c"], "must process in send order");
        assert!(reg.lock().await.is_empty(), "all inflight entries must be removed after processing");
    }

    /// A turn whose token was cancelled BEFORE the consumer reaches it (e.g. a
    /// `Cancel` raced ahead while queued) is skipped, never run, and its entry
    /// is removed.
    #[tokio::test]
    async fn consumer_skips_turn_cancelled_while_queued() {
        let reg = inflight();
        let (tx, rx) = mpsc::unbounded_channel::<TestTurn>();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let first = make_turn(&reg, "cancelled");
        first.cancel_token().cancel(); // simulate Cancel racing ahead of the consumer
        let second = make_turn(&reg, "runs");

        let recorder = seen.clone();
        let handle = tokio::spawn(consumer(rx, move |turn: TestTurn| {
            let recorder = recorder.clone();
            async move {
                recorder.lock().await.push(turn.request_id().to_string());
            }
        }));

        tx.send(first).unwrap();
        tx.send(second).unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(*seen.lock().await, vec!["runs"], "cancelled-while-queued turn must be skipped, not run");
        assert!(reg.lock().await.is_empty(), "both entries must be removed (skip path removes explicitly)");
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

    /// The primitive `enqueue`'s dead-consumer retry loop relies on: once the
    /// receiver end is dropped (simulating a consumer that panicked/exited),
    /// the sender reports `is_closed() == true`, and `remove_if` keyed on that
    /// predicate evicts the stale entry so the next `entry(...).or_insert_with`
    /// creates a fresh live sender. Exercised at the map level (no
    /// `AgentEngine` needed) since the production `enqueue`/`consumer` require
    /// a real engine to build a `QueuedTurn`.
    #[tokio::test]
    async fn dead_sender_is_detected_and_evicted() {
        let map: DashMap<SessionKey, mpsc::UnboundedSender<()>> = DashMap::new();
        let key = SessionKey {
            agent_name: "agent".to_string(),
            eff_user: "user".to_string(),
            eff_channel: "telegram".to_string(),
            eff_chat_scope: None,
        };

        let (tx, rx) = mpsc::unbounded_channel::<()>();
        map.insert(key.clone(), tx.clone());
        drop(rx); // simulate the consumer task having exited

        assert!(tx.is_closed(), "sender must report closed once its receiver is dropped");
        assert!(map.contains_key(&key), "stale entry is still present before eviction");

        map.remove_if(&key, |_, v| v.is_closed());
        assert!(!map.contains_key(&key), "remove_if must evict the closed sender");

        // A fresh entry() after eviction creates a brand-new live sender, as
        // `enqueue`'s retry loop does on its next iteration.
        let (tx2, _rx2) = mpsc::unbounded_channel::<()>();
        map.entry(key.clone()).or_insert(tx2);
        assert!(!map.get(&key).unwrap().is_closed(), "replacement sender must be live");
    }
}
