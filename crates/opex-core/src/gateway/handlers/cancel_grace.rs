//! Cancel-grace polling helper for the SSE response loop in `chat.rs`.
//!
//! When a user clicks Stop (or an external `/api/chat/{sid}/abort` fires),
//! the backend engine does not get hard-aborted immediately — it gets a
//! bounded window to emit its final aborted-message row + `Finish` event
//! naturally. If the engine wedges inside a tool loop or sync block that
//! ignores the cancel token, we must eventually force an abort so the
//! tokio task and its semaphore permit are freed.
//!
//! This helper factors that logic out of the large `chat.rs` SSE loop so
//! the deadline behavior can be unit-tested with `tokio::time::pause()` /
//! `advance()` — without spinning up the full handler.
//!
//! Contract:
//! * Before cancel is observed, `deadline` is `None` → behave like
//!   `rx.recv().await` with two outcomes: `Event(e)` or `Closed`.
//! * Once cancel is observed the caller sets `deadline = Some(now +
//!   grace)`. This helper then races `rx.recv()` against the deadline:
//!   if an event arrives in time, `Event(e)`; if the channel closes in
//!   time, `Closed`; if the deadline passes with nothing, `GraceExceeded`
//!   and the caller hard-aborts the engine.

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

/// Result of `poll_event_with_cancel_grace`.
///
/// `T` is the event type the SSE loop receives — in practice
/// `StreamEvent`, but the helper is generic over `T` so unit tests can
/// use a simple stand-in type.
#[derive(Debug)]
pub(crate) enum GracePollResult<T> {
    /// An event arrived before the deadline (or before any deadline was
    /// set).
    Event(T),
    /// The sender half of the channel was dropped (engine finished
    /// naturally). Caller should break out of the loop.
    Closed,
    /// The deadline passed with neither an event nor a channel close.
    /// Caller should hard-abort the engine and break.
    GraceExceeded,
}

/// Poll `rx` respecting an optional cancel-grace deadline.
///
/// * `deadline == None` → unbounded `rx.recv()`.
/// * `deadline == Some(dl)` → race `rx.recv()` against `timeout_at(dl)`.
pub(crate) async fn poll_event_with_cancel_grace<T>(
    rx: &mut UnboundedReceiver<T>,
    deadline: Option<Instant>,
) -> GracePollResult<T> {
    if let Some(dl) = deadline {
        match tokio::time::timeout_at(dl, rx.recv()).await {
            Ok(Some(ev)) => GracePollResult::Event(ev),
            Ok(None) => GracePollResult::Closed,
            Err(_) => GracePollResult::GraceExceeded,
        }
    } else {
        match rx.recv().await {
            Some(ev) => GracePollResult::Event(ev),
            None => GracePollResult::Closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// Without a deadline, the helper behaves like a plain `recv()`.
    #[tokio::test]
    async fn no_deadline_yields_event() {
        let (tx, mut rx) = mpsc::unbounded_channel::<u32>();
        tx.send(42).unwrap();
        let result = poll_event_with_cancel_grace(&mut rx, None).await;
        assert!(matches!(result, GracePollResult::Event(42)));
    }

    /// Without a deadline, channel close propagates as `Closed`.
    #[tokio::test]
    async fn no_deadline_yields_closed_when_tx_dropped() {
        let (tx, mut rx) = mpsc::unbounded_channel::<u32>();
        drop(tx);
        let result = poll_event_with_cancel_grace(&mut rx, None).await;
        assert!(matches!(result, GracePollResult::Closed));
    }

    /// With a deadline in the future, an event arriving before the
    /// deadline still yields `Event`.
    #[tokio::test(start_paused = true)]
    async fn deadline_in_future_yields_event_if_arrives_in_time() {
        let (tx, mut rx) = mpsc::unbounded_channel::<u32>();
        let deadline = Instant::now() + Duration::from_secs(30);

        // Send BEFORE awaiting — ensures event is ready.
        tx.send(7).unwrap();

        let result = poll_event_with_cancel_grace(&mut rx, Some(deadline)).await;
        assert!(matches!(result, GracePollResult::Event(7)));
    }

    /// Deadline passes with no events → `GraceExceeded`. Uses
    /// `tokio::time::pause()` to make the test deterministic — no
    /// real wall-clock wait.
    #[tokio::test(start_paused = true)]
    async fn deadline_exceeded_returns_grace_exceeded() {
        let (tx, mut rx) = mpsc::unbounded_channel::<u32>();
        let deadline = Instant::now() + Duration::from_secs(30);

        // Keep tx alive so the channel stays open — we want the deadline
        // to fire, not the channel to close.
        let _keep_alive = tx;

        // Advance virtual time past the deadline while the helper is
        // pending. `tokio::time::timeout_at` wakes on the deadline.
        let helper = tokio::spawn(async move {
            poll_event_with_cancel_grace(&mut rx, Some(deadline)).await
        });
        tokio::time::advance(Duration::from_secs(31)).await;
        let result = helper.await.unwrap();

        assert!(matches!(result, GracePollResult::GraceExceeded));
    }

    /// If the channel closes before the deadline, we get `Closed`
    /// (engine finished in time).
    #[tokio::test(start_paused = true)]
    async fn tx_dropped_before_deadline_yields_closed() {
        let (tx, mut rx) = mpsc::unbounded_channel::<u32>();
        let deadline = Instant::now() + Duration::from_secs(30);
        drop(tx);

        let result = poll_event_with_cancel_grace(&mut rx, Some(deadline)).await;
        assert!(matches!(result, GracePollResult::Closed));
    }

    /// Deadline in the past fires immediately.
    #[tokio::test(start_paused = true)]
    async fn deadline_already_passed_returns_grace_exceeded_immediately() {
        let (tx, mut rx) = mpsc::unbounded_channel::<u32>();
        let _keep_alive = tx;
        // Create deadline first, then advance time past it BEFORE calling
        // the helper. `timeout_at` with an already-elapsed deadline fires
        // synchronously on the first poll.
        let deadline = Instant::now() + Duration::from_millis(10);
        tokio::time::advance(Duration::from_millis(20)).await;

        let result = poll_event_with_cancel_grace(&mut rx, Some(deadline)).await;
        assert!(matches!(result, GracePollResult::GraceExceeded));
    }
}
