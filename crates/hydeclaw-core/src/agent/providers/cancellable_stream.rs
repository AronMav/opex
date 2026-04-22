use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use super::error::CancelReason;

/// Single-writer slot for the reason a stream was cancelled.
/// Writers MUST `set()` BEFORE cancelling the token, so readers that
/// wake on `token.cancelled()` always see a populated reason.
#[derive(Debug, Clone)]
pub struct CancelSlot(Arc<OnceCell<CancelReason>>);

impl CancelSlot {
    pub fn new() -> Self {
        Self(Arc::new(OnceCell::new()))
    }

    /// Returns Ok if this writer won the race, Err(losing_reason) if another writer already set the slot.
    pub fn set(&self, reason: CancelReason) -> Result<(), CancelReason> {
        self.0.set(reason).map_err(|_| reason)
    }

    pub fn get(&self) -> Option<CancelReason> {
        self.0.get().copied()
    }
}

impl Default for CancelSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience: set reason, then cancel. Correct ordering for the
/// "readers always see a populated reason" invariant.
pub fn set_and_cancel(slot: &CancelSlot, token: &CancellationToken, reason: CancelReason) {
    let _ = slot.set(reason);
    token.cancel();
}

/// Materializes the "shutdown drain" classification before the root
/// cancellation token is cancelled. The drain handler MUST call this
/// helper BEFORE `token.cancel()` so the `cancellable_stream` producer
/// observes `ShutdownDrain` rather than defaulting to `UserCancelled`
/// when the external-cancel arm wakes.
///
/// Safe to call repeatedly; `OnceCell` semantics mean subsequent calls
/// are silent no-ops (first winner wins).
///
/// `#[allow(dead_code)]`: no runtime caller today — this helper ships with
/// the public contract + test coverage in place so the eventual
/// `shutdown::drain_active_streams` handler has a typed entrypoint.
#[allow(dead_code)]
pub fn set_shutdown_drain_reason(slot: &CancelSlot, token: &CancellationToken) {
    set_and_cancel(slot, token, CancelReason::ShutdownDrain);
}

use async_stream::stream;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::time::Duration;

use super::timeouts::TimeoutsConfig;

/// Wrap an inner byte stream with cancellation + inactivity-timer.
///
/// Implementation detail: a background producer task drains `inner` into an
/// internal mpsc channel so that the inactivity timer fires even when no
/// consumer is actively polling the returned stream. Task 10 extends this
/// with max-duration and further decouples the producer from the caller.
pub fn stream_with_cancellation<S>(
    mut inner: S,
    cancel: CancellationToken,
    slot: CancelSlot,
    timeouts: TimeoutsConfig,
) -> impl Stream<Item = Result<Bytes, reqwest::Error>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Bytes, reqwest::Error>>(8);
    let producer_cancel = cancel.clone();
    let producer_slot = slot.clone();
    let inactivity_secs = timeouts.stream_inactivity_secs;
    let inactivity = Duration::from_secs(inactivity_secs.max(1));
    let inactivity_enabled = inactivity_secs > 0;
    let max_duration_secs = timeouts.stream_max_duration_secs;
    let max_duration = Duration::from_secs(max_duration_secs.max(1));
    let max_duration_enabled = max_duration_secs > 0;
    let start = tokio::time::Instant::now();

    tokio::spawn(async move {
        loop {
            let next = async {
                if inactivity_enabled {
                    match tokio::time::timeout(inactivity, inner.next()).await {
                        Ok(v) => v.map(Ok),
                        Err(_) => Some(Err(())),
                    }
                } else {
                    inner.next().await.map(Ok)
                }
            };

            tokio::select! {
                _ = producer_cancel.cancelled() => {
                    // External cancel (user Stop, shutdown drain, or engine-level abort).
                    // Timers fire via `set_and_cancel`, so if the slot already holds a
                    // reason (InactivityTimeout / MaxDurationExceeded / ShutdownDrain
                    // via `set_shutdown_drain_reason`) we MUST NOT overwrite it —
                    // `OnceCell::set` would Err but we keep the guard explicit for
                    // clarity and for the case where the caller pre-set the slot.
                    // Only default to UserCancelled when the slot is still empty.
                    if producer_slot.get().is_none() {
                        let _ = producer_slot.set(CancelReason::UserCancelled);
                    }
                    break;
                }
                _ = tokio::time::sleep_until(start + max_duration), if max_duration_enabled => {
                    set_and_cancel(
                        &producer_slot,
                        &producer_cancel,
                        CancelReason::MaxDurationExceeded { elapsed_secs: max_duration_secs },
                    );
                    break;
                }
                v = next => {
                    match v {
                        Some(Ok(Ok(bytes))) => {
                            if tx.send(Ok(bytes)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Err(e))) => {
                            let _ = tx.send(Err(e)).await;
                            break;
                        }
                        Some(Err(())) => {
                            set_and_cancel(
                                &producer_slot,
                                &producer_cancel,
                                CancelReason::InactivityTimeout { silent_secs: inactivity_secs },
                            );
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
    });

    // Consumer stream: drains the mpsc without racing `cancel.cancelled()`.
    // The producer task above is the SOLE writer to `slot`; when it handles
    // cancel it drops `tx`, and the consumer reads `None` from `rx.recv()`.
    // That ordering guarantees `slot.get()` is populated by the time the
    // stream yields EOF to the caller, so downstream providers that read
    // `slot` after the stream ends never observe the window where the token
    // was cancelled but the slot had not yet been set. Racing the token
    // here directly reintroduced that gap (consumer could break before the
    // producer scheduled) — see commit log for the regression history.
    stream! {
        while let Some(item) = rx.recv().await {
            match item {
                Ok(bytes) => yield Ok(bytes),
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_writer_wins() {
        let slot = CancelSlot::new();
        assert!(slot.set(CancelReason::UserCancelled).is_ok());
        assert_eq!(
            slot.set(CancelReason::ShutdownDrain),
            Err(CancelReason::ShutdownDrain)
        );
        assert_eq!(slot.get(), Some(CancelReason::UserCancelled));
    }

    #[tokio::test]
    async fn set_and_cancel_orders_writes() {
        let slot = CancelSlot::new();
        let token = CancellationToken::new();

        let observer_slot = slot.clone();
        let observer_token = token.clone();
        let task = tokio::spawn(async move {
            observer_token.cancelled().await;
            observer_slot.get()
        });

        set_and_cancel(
            &slot,
            &token,
            CancelReason::MaxDurationExceeded { elapsed_secs: 600 },
        );
        let reason = task.await.unwrap();
        assert!(matches!(
            reason,
            Some(CancelReason::MaxDurationExceeded { elapsed_secs: 600 })
        ));
    }

    #[tokio::test]
    async fn race_only_first_wins() {
        let slot = CancelSlot::new();
        let token = CancellationToken::new();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = slot.clone();
                let t = token.clone();
                tokio::spawn(async move {
                    let reason = if i % 2 == 0 {
                        CancelReason::UserCancelled
                    } else {
                        CancelReason::ShutdownDrain
                    };
                    set_and_cancel(&s, &t, reason);
                })
            })
            .collect();

        for h in handles {
            h.await.unwrap();
        }
        assert!(slot.get().is_some());
    }

    use bytes::Bytes;
    use futures_util::StreamExt;
    use std::time::Duration;
    use tokio_stream::wrappers::ReceiverStream;

    fn chunk_stream(chunks: Vec<(&'static str, Duration)>) -> impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Unpin {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, reqwest::Error>>(8);
        tokio::spawn(async move {
            for (s, d) in chunks {
                tokio::time::sleep(d).await;
                if tx.send(Ok(Bytes::from(s))).await.is_err() { return; }
            }
            // Keep `tx` alive so the stream stays open (source silent, not ended).
            // The inactivity timer is what should drive cancellation, not EOF.
            futures_util::future::pending::<()>().await;
        });
        ReceiverStream::new(rx)
    }

    /// Like `chunk_stream` but closes `tx` after the last chunk, so `inner.next()`
    /// returns `None` (clean EOF). Used to test backpressure / consumer-anchored
    /// scenarios where the network source has completed delivery.
    fn chunk_stream_ending(chunks: Vec<(&'static str, Duration)>) -> impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Unpin {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, reqwest::Error>>(8);
        tokio::spawn(async move {
            for (s, d) in chunks {
                tokio::time::sleep(d).await;
                if tx.send(Ok(Bytes::from(s))).await.is_err() { return; }
            }
            // Drop tx — stream EOF. Producer task reads None and exits without
            // firing the inactivity timer.
        });
        ReceiverStream::new(rx)
    }

    #[tokio::test]
    async fn inactivity_timer_fires_when_source_silent() {
        let token = CancellationToken::new();
        let slot = CancelSlot::new();
        let timeouts = crate::agent::providers::TimeoutsConfig {
            connect_secs: 10,
            request_secs: 120,
            stream_inactivity_secs: 1,      // 1s to keep test fast
            stream_max_duration_secs: 3600, // effectively off
            run_max_duration_secs: 0,
        };
        let inner = chunk_stream(vec![
            ("hello", Duration::from_millis(100)),
            // Then nothing — source stays silent.
        ]);
        let mut out = Box::pin(stream_with_cancellation(inner, token.clone(), slot.clone(), timeouts));

        // First chunk arrives.
        let first = tokio::time::timeout(Duration::from_millis(500), out.next()).await.unwrap();
        assert!(matches!(first, Some(Ok(_))));

        // Now wait for inactivity to trigger cancellation.
        tokio::time::timeout(Duration::from_secs(3), token.cancelled()).await
            .expect("token must be cancelled after stream_inactivity_secs");

        assert!(matches!(slot.get(), Some(CancelReason::InactivityTimeout { silent_secs: _ })));
    }

    #[tokio::test]
    async fn inactivity_timer_resets_on_each_chunk() {
        let token = CancellationToken::new();
        let slot = CancelSlot::new();
        let timeouts = crate::agent::providers::TimeoutsConfig {
            connect_secs: 10,
            request_secs: 120,
            stream_inactivity_secs: 1,
            stream_max_duration_secs: 3600,
            run_max_duration_secs: 0,
        };
        // 5 chunks, each 400 ms apart — each arrives well under the 1 s limit.
        let inner = chunk_stream((0..5).map(|_| ("x", Duration::from_millis(400))).collect());
        let mut out = Box::pin(stream_with_cancellation(inner, token.clone(), slot.clone(), timeouts));

        for _ in 0..5 {
            let item = tokio::time::timeout(Duration::from_secs(2), out.next()).await.unwrap();
            assert!(matches!(item, Some(Ok(_))));
        }
        assert!(slot.get().is_none(), "timer must not have fired");
    }

    #[tokio::test]
    async fn max_duration_timer_fires_even_when_stream_is_active() {
        let token = CancellationToken::new();
        let slot = CancelSlot::new();
        let timeouts = crate::agent::providers::TimeoutsConfig {
            connect_secs: 10,
            request_secs: 120,
            stream_inactivity_secs: 60,
            stream_max_duration_secs: 1, // 1s wall clock
            run_max_duration_secs: 0,
        };
        // Chunks every 100 ms for 3 seconds (30 total) — inactivity never fires,
        // but max_duration must trigger around 1s.
        let inner = chunk_stream(
            (0..30).map(|_| ("x", Duration::from_millis(100))).collect()
        );
        let out = stream_with_cancellation(inner, token.clone(), slot.clone(), timeouts);
        let mut out = Box::pin(out);

        tokio::time::timeout(Duration::from_secs(3), async {
            while (out.next().await).is_some() {}
        }).await.ok();

        assert!(matches!(slot.get(), Some(CancelReason::MaxDurationExceeded { elapsed_secs: _ })));
    }

    #[tokio::test]
    async fn slow_consumer_does_not_trigger_inactivity() {
        // Proves §4.5.1: inactivity is anchored to the NETWORK producer,
        // not the consumer. A slow consumer must NOT see InactivityTimeout
        // if the network source is still delivering chunks.
        let token = CancellationToken::new();
        let slot = CancelSlot::new();
        let timeouts = crate::agent::providers::TimeoutsConfig {
            connect_secs: 10,
            request_secs: 120,
            stream_inactivity_secs: 1,
            stream_max_duration_secs: 3600,
            run_max_duration_secs: 0,
        };

        // Source sends 5 chunks quickly (50 ms apart) then closes cleanly.
        // This models a finished HTTP response — producer reads None and exits
        // without firing inactivity.
        let inner = chunk_stream_ending(
            (0..5).map(|_| ("x", Duration::from_millis(50))).collect()
        );
        let out = stream_with_cancellation(inner, token.clone(), slot.clone(), timeouts);
        let mut out = Box::pin(out);

        // Consumer is slow — 400 ms per chunk for 5 chunks = 2 s total.
        // If the timer were anchored to consumer poll_next, it would fire;
        // it must NOT fire because the producer has long since delivered
        // all chunks into the mpsc.
        for _ in 0..5 {
            let _ = out.next().await;
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        assert!(slot.get().is_none(), "slow consumer must not trigger inactivity");
    }

    /// Regression: consumer used to race `cancel.cancelled()` in its own
    /// `select!`, which could break before the producer's cancel arm wrote
    /// the slot. The provider would then read `slot.get() == None` and
    /// silently reclassify the user cancel as a successful empty response.
    /// After the fix the consumer only drains `rx.recv()`, and the producer
    /// (sole writer) always sets the slot before dropping `tx`.
    #[tokio::test]
    async fn producer_writes_slot_before_consumer_sees_eof_on_external_cancel() {
        // Run the scenario many times to catch intermittent ordering bugs.
        for _ in 0..32 {
            let token = CancellationToken::new();
            let slot = CancelSlot::new();
            let timeouts = crate::agent::providers::TimeoutsConfig {
                connect_secs: 10,
                request_secs: 120,
                stream_inactivity_secs: 60,
                stream_max_duration_secs: 3600,
                run_max_duration_secs: 0,
            };
            // Source streams forever (pending) — no natural EOF.
            let inner = chunk_stream(vec![("alpha", Duration::from_millis(10))]);
            let out = stream_with_cancellation(inner, token.clone(), slot.clone(), timeouts);
            let mut out = Box::pin(out);

            // Receive the first chunk so the stream is live.
            let _ = tokio::time::timeout(Duration::from_millis(200), out.next())
                .await
                .expect("first chunk must arrive");

            // External cancel from an outside task.
            token.cancel();

            // Drain the consumer — it must reach EOF (None) because the
            // producer drops `tx` after setting the slot.
            while out.next().await.is_some() {}

            // By the time the consumer saw EOF, the slot MUST be populated.
            assert!(
                slot.get().is_some(),
                "producer must have written the slot before consumer EOF"
            );
            assert_eq!(
                slot.get(),
                Some(CancelReason::UserCancelled),
                "external cancel without a pre-written slot classifies as UserCancelled"
            );
        }
    }

    /// Regression: the cancel arm used to unconditionally `slot.set(UserCancelled)`
    /// which, while technically safe (OnceCell::set Err), conveyed the wrong
    /// intent. The guarded form keeps pre-set reasons (e.g. `ShutdownDrain`
    /// wired by a drain handler before `token.cancel()`) intact.
    #[tokio::test]
    async fn cancel_arm_preserves_pre_set_shutdown_drain() {
        let token = CancellationToken::new();
        let slot = CancelSlot::new();
        let timeouts = crate::agent::providers::TimeoutsConfig {
            connect_secs: 10,
            request_secs: 120,
            stream_inactivity_secs: 60,
            stream_max_duration_secs: 3600,
            run_max_duration_secs: 0,
        };
        let inner = chunk_stream(vec![("x", Duration::from_millis(10))]);
        let out = stream_with_cancellation(inner, token.clone(), slot.clone(), timeouts);
        let mut out = Box::pin(out);
        let _ = tokio::time::timeout(Duration::from_millis(200), out.next())
            .await
            .unwrap();

        // Drain handler wires the reason BEFORE cancelling the token.
        set_shutdown_drain_reason(&slot, &token);

        // Consumer drains naturally; producer should observe token cancelled
        // but slot already set → skips the UserCancelled default.
        while out.next().await.is_some() {}

        assert_eq!(
            slot.get(),
            Some(CancelReason::ShutdownDrain),
            "pre-set ShutdownDrain must survive the cancel arm's default"
        );
    }

    /// `set_shutdown_drain_reason` is the public helper callers use so they
    /// don't have to know the slot-before-cancel ordering rule.
    #[tokio::test]
    async fn set_shutdown_drain_reason_sets_then_cancels() {
        let slot = CancelSlot::new();
        let token = CancellationToken::new();

        let observer_slot = slot.clone();
        let observer_token = token.clone();
        let task = tokio::spawn(async move {
            observer_token.cancelled().await;
            observer_slot.get()
        });

        set_shutdown_drain_reason(&slot, &token);
        let reason = task.await.unwrap();
        assert_eq!(reason, Some(CancelReason::ShutdownDrain));
    }
}
