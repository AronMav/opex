//! Phase 62 RES-01: SOLE engine-side send surface for StreamEvent.
//!
//! Contract (pinned by 62-CONTEXT.md locked decisions):
//!   * TextDelta may be dropped under saturation (the coalescer drop counter
//!     records this). Engine task MUST NOT block on text-delta sends.
//!   * Every other StreamEvent variant (Finish, Error, File, RichCard,
//!     AgentSwitch, Approval*, ToolCall*, Step*, MessageStart, SessionId)
//!     is NEVER coalesced or dropped under normal operation.
//!
//! # API shape
//!
//! Two entry points, both preserving the original `.send(ev)` call-style
//! ergonomics at the engine call site:
//!
//!   * `send(ev)` — **synchronous, non-blocking** (for today's sync call sites
//!     that exist inside `async fn`s but do not use `.await` on the send). Uses
//!     `try_send` under the hood. On Full for non-TextDelta, returns
//!     `Err(FullNonText(ev))` so the caller can log/retry — silent drop is NOT
//!     performed here; dropping non-text would violate the CONTEXT.md contract.
//!   * `send_async(ev).await` — **async, respects the CONTEXT.md contract
//!     end-to-end.** TextDelta uses `try_send` (droppable). Every other
//!     variant uses `self.inner.send(ev).await`, which awaits a slot and
//!     only errors if the channel is closed.
//!
//! # Why not `blocking_send`?
//!
//! `tokio::sync::mpsc::Sender::blocking_send` **panics when called from
//! within an asynchronous execution context** (see tokio docs). The engine's
//! `handle_sse` function and every helper it spawns run inside tokio tasks,
//! so `blocking_send` would crash the agent. `send_async(ev).await` is the
//! non-panicking equivalent — it suspends the task instead of blocking the
//! thread, which is the correct idiom for async Rust.
//!
//! # Saturation behavior
//!
//! The engine-side bounded channel (capacity 256 in chat.rs) is drained by
//! the coalescer task at a 16 ms cadence. Under realistic load, Full is
//! vanishingly rare — the coalescer's main cost is the 16 ms tick, and one
//! tick at 256-item buffer yields 16_000 evts/s steady-state which exceeds
//! the fastest provider's token rate by orders of magnitude. Drops will
//! therefore occur only during synthetic burst tests or pathological
//! backpressure. The `send()` sync entry reports FullNonText explicitly so
//! engine callers (currently `handle_sse` et al.) can choose to log, retry,
//! or upgrade to `send_async(ev).await` for hard never-drop guarantees.

use tokio::sync::mpsc;

use crate::agent::engine::StreamEvent;

/// Result of an engine-side send.
#[derive(Debug)]
#[allow(dead_code)] // variants inspected by callers via Debug + pattern-match in tests
pub enum EngineSendError {
    /// The channel is closed (receiver gone). Contains the event that was dropped.
    Closed(StreamEvent),
    /// Text-delta only: channel was Full, event dropped per CONTEXT.md contract.
    DroppedTextDelta,
    /// Non-text-delta event: channel was Full on a sync `send()`. Caller MUST
    /// retry via `send_async(ev).await` or another path — this event has NOT
    /// been delivered, and dropping it silently would violate the CONTEXT.md
    /// "never coalesce or drop non-text" contract. Contains the event for retry.
    FullNonText(StreamEvent),
}

/// Thin wrapper around `mpsc::Sender<StreamEvent>` enforcing the Phase 62
/// RES-01 contract: text-delta is droppable, everything else is not.
///
/// The `Err` variants of `send`/`send_async` intentionally carry the original
/// `StreamEvent` so callers can retry (for `FullNonText`) or log detailed
/// diagnostics (for `Closed`). `clippy::result_large_err` is silenced at the
/// impl block below — every call site either ignores the Err via `.ok()` /
/// `let _ = …` or pattern-matches on the enum, so the 128-byte payload is
/// cold-path only and never copied into hot-loop error propagation.
#[derive(Clone)]
pub struct EngineEventSender {
    inner: mpsc::Sender<StreamEvent>,
}

#[allow(clippy::result_large_err)] // Err variants carry StreamEvent by design for retry paths.
impl EngineEventSender {
    pub fn new(inner: mpsc::Sender<StreamEvent>) -> Self {
        Self { inner }
    }

    /// Access the inner `mpsc::Sender` for interop with call sites that
    /// manage their own forwarding (e.g. spawned forwarder tasks that move
    /// a sender into a separate tokio task). The inner sender retains the
    /// `try_send`/`send().await` API directly — callers that reach for
    /// this escape hatch take responsibility for the CONTEXT.md contract.
    #[inline]
    #[allow(dead_code)] // escape hatch retained for future forwarder integrations
    pub fn inner(&self) -> &mpsc::Sender<StreamEvent> {
        &self.inner
    }

    /// Non-async send using `try_send` under the hood.
    ///
    /// * **TextDelta:** on Full returns `Err(DroppedTextDelta)` (contract-compliant drop).
    /// * **All other variants:** on Full returns `Err(FullNonText(ev))` — the event
    ///   has NOT been delivered. Caller must retry via `send_async(ev).await`
    ///   or surface the error upward. A silent drop here would violate the
    ///   CONTEXT.md "non-text never dropped" locked decision.
    /// * **On closed channel:** returns `Err(Closed(ev))` for all variants.
    ///
    /// Prefer `send_async` in async contexts — it preserves the never-drop
    /// contract for non-text events without requiring a retry loop.
    #[allow(dead_code)] // used in tests and retained for callers that need sync send
    pub fn send(&self, ev: StreamEvent) -> Result<(), EngineSendError> {
        match self.inner.try_send(ev) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(ev)) => Err(EngineSendError::Closed(ev)),
            Err(mpsc::error::TrySendError::Full(ev)) => {
                if matches!(ev, StreamEvent::TextDelta(_)) {
                    Err(EngineSendError::DroppedTextDelta)
                } else {
                    Err(EngineSendError::FullNonText(ev))
                }
            }
        }
    }

    /// Async send honoring the CONTEXT.md contract end-to-end.
    ///
    /// * **TextDelta:** uses `try_send` (non-blocking); on Full returns
    ///   `Err(DroppedTextDelta)`. Engine task NEVER blocks on text-delta sends.
    /// * **All other variants:** uses `self.inner.send(ev).await`, which awaits
    ///   a free slot in the bounded channel. Guaranteed delivery unless the
    ///   receiver was dropped (returns `Err(Closed(ev))`).
    ///
    /// This is the preferred engine-side entry point — it preserves the
    /// "non-text never dropped" contract without requiring the caller to
    /// write a retry loop.
    pub async fn send_async(&self, ev: StreamEvent) -> Result<(), EngineSendError> {
        match ev {
            StreamEvent::TextDelta(_) => match self.inner.try_send(ev) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Closed(ev)) => Err(EngineSendError::Closed(ev)),
                Err(mpsc::error::TrySendError::Full(_)) => Err(EngineSendError::DroppedTextDelta),
            },
            _ => self
                .inner
                .send(ev)
                .await
                .map_err(|mpsc::error::SendError(ev)| EngineSendError::Closed(ev)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::result_large_err)]
    async fn text_delta_drops_on_full() {
        let (tx, _rx) = mpsc::channel::<StreamEvent>(1);
        let sender = EngineEventSender::new(tx);
        // First send fills the slot.
        let h1 = tokio::task::spawn_blocking({
            let s = sender.clone();
            move || s.send(StreamEvent::TextDelta("a".into()))
        });
        assert!(h1.await.unwrap().is_ok());
        // Second text-delta must drop (channel is Full, _rx never drains).
        let h2 = tokio::task::spawn_blocking({
            let s = sender.clone();
            move || s.send(StreamEvent::TextDelta("b".into()))
        });
        let res = h2.await.unwrap();
        assert!(
            matches!(res, Err(EngineSendError::DroppedTextDelta)),
            "text-delta must drop on Full; got {res:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn non_text_blocks_never_drops_on_full_via_send_async() {
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(1);
        let sender = EngineEventSender::new(tx);
        // Fill the slot with TextDelta (which succeeds via try_send).
        sender
            .send_async(StreamEvent::TextDelta("filler".into()))
            .await
            .expect("filler");

        // Spawn an async send_async of Error — this should await until rx drains.
        let h = tokio::spawn({
            let s = sender.clone();
            async move { s.send_async(StreamEvent::Error("boom".into())).await }
        });

        // Yield so the send_async call starts.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Drain one slot to unblock the Error send.
        let _ = rx.recv().await.expect("filler");
        // The Error send must now succeed (NOT drop).
        let res = h.await.unwrap();
        assert!(res.is_ok(), "Error event must not drop on Full; got {res:?}");
        // Confirm Error arrived.
        let got = rx.recv().await.expect("error delivered");
        assert!(matches!(got, StreamEvent::Error(ref s) if s == "boom"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_text_sync_returns_fullnon_text_on_full() {
        let (tx, _rx) = mpsc::channel::<StreamEvent>(1);
        let sender = EngineEventSender::new(tx);
        // Fill the slot with TextDelta.
        sender.send(StreamEvent::TextDelta("a".into())).expect("filler");
        // Second send (non-text) on Full: sync path surfaces FullNonText so
        // the caller can retry. This preserves the CONTEXT.md contract —
        // the wrapper refuses to silently drop non-text events.
        let res = sender.send(StreamEvent::Error("retry me".into()));
        assert!(
            matches!(res, Err(EngineSendError::FullNonText(StreamEvent::Error(_)))),
            "non-text sync Full must surface FullNonText; got {res:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_text_returns_closed_when_rx_dropped() {
        let (tx, rx) = mpsc::channel::<StreamEvent>(4);
        drop(rx);
        let sender = EngineEventSender::new(tx);
        let res = sender.send_async(StreamEvent::Error("x".into())).await;
        assert!(
            matches!(res, Err(EngineSendError::Closed(_))),
            "Error on closed channel must return Closed; got {res:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn all_non_text_variants_use_blocking_path_via_send_async() {
        // Regression guard: enumerate every non-text-delta variant and assert
        // send_async delivers it via the `send().await` path (never drops).
        // If a new StreamEvent variant is added and forgotten in
        // engine_event_sender, this test still catches Closed/DroppedTextDelta
        // surfacing for a non-text event.
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let sender = EngineEventSender::new(tx);

        let cases = vec![
            StreamEvent::SessionId("s1".into()),
            StreamEvent::MessageStart { message_id: "m1".into() },
            StreamEvent::StepStart { step_id: "st1".into() },
            StreamEvent::ToolCallStart { id: "t1".into(), name: "tool".into() },
            StreamEvent::ToolCallArgs { id: "t1".into(), args_text: "{}".into() },
            StreamEvent::ToolResult { id: "t1".into(), result: "ok".into() },
            StreamEvent::StepFinish { step_id: "st1".into(), finish_reason: "stop".into() },
            StreamEvent::RichCard { card_type: "table".into(), data: serde_json::json!({}) },
            StreamEvent::File { url: "u".into(), media_type: "image/png".into() },
            StreamEvent::AgentSwitch { agent_name: "a".into() },
            StreamEvent::ApprovalNeeded {
                approval_id: "a1".into(),
                tool_name: "tool".into(),
                tool_input: serde_json::json!({}),
                timeout_ms: 1000,
            },
            StreamEvent::ApprovalResolved {
                approval_id: "a1".into(),
                action: "approved".into(),
                modified_input: None,
            },
            StreamEvent::Finish { finish_reason: "stop".into(), continuation: false },
            StreamEvent::Error("err".into()),
        ];
        for ev in cases {
            sender.send_async(ev).await.expect("non-text must deliver");
        }
        // Drain to confirm all 14 non-text events arrived.
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 14, "all 14 non-text variants must be delivered");
    }
}
