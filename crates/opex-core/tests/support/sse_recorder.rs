//! In-process SSE-channel pair fixture mirroring chat.rs's bounded-outer +
//! unbounded-inner pattern. Decoupled from engine internals — declares its
//! own TestStreamEvent enum with the minimal variants needed for Phase 61
//! characterization.
//!
//! Lifecycle contract:
//!   - `SseRecorder::new()` returns `(recorder, snapshot_handle)`.
//!   - The caller drives the producer side via `recorder.send(...)`.
//!   - Dropping `recorder` closes the inner sender. The internal converter
//!     task drains, forwards remaining events to the outer channel, then
//!     exits. The recorder task drains the outer and pushes into the Vec,
//!     then exits. At that point `snapshot_handle.await` resolves to the
//!     final Vec — the snapshot is GUARANTEED to be the post-drain state.
//!   - This is why the natural-finish test asserts `snapshot.last()` rather
//!     than fixed positions: order is preserved, but the snapshot is taken
//!     after the recorder fully drains, not mid-stream.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

/// Local stream-event enum used by the SSE characterization tests.
/// Mirrors the SHAPE (not the type) of `crate::agent::engine::StreamEvent`
/// — production-code coupling is intentionally avoided in Phase 61.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStreamEvent {
    TextDelta(String),
    Finish,
    Error(String),
}

/// Fixture that wires inner-unbounded → converter → outer-bounded → recorder Vec.
///
/// Hold the returned `SseRecorder` to drive the producer. When you drop it,
/// the inner channel closes and the snapshot JoinHandle resolves with the
/// final fully-drained Vec.
pub struct SseRecorder {
    inner_tx: mpsc::UnboundedSender<TestStreamEvent>,
    outer_capacity: usize,
}

impl SseRecorder {
    /// Build a fixture and spawn the converter + recorder tasks.
    ///
    /// Returns (recorder, snapshot_handle). The snapshot handle resolves with
    /// the post-drain Vec once the recorder is dropped (or otherwise closes).
    pub fn new() -> (Self, JoinHandle<Vec<TestStreamEvent>>) {
        Self::with_outer_capacity(8)
    }

    /// Variant with explicit outer-channel capacity for saturation tests.
    pub fn with_outer_capacity(outer_capacity: usize) -> (Self, JoinHandle<Vec<TestStreamEvent>>) {
        let (inner_tx, mut inner_rx) = mpsc::unbounded_channel::<TestStreamEvent>();
        let (outer_tx, mut outer_rx) = mpsc::channel::<TestStreamEvent>(outer_capacity);

        let recorded: Arc<Mutex<Vec<TestStreamEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_for_recorder = Arc::clone(&recorded);

        // Converter: drain inner, forward to outer (await on send — replicates
        // chat.rs current behavior of `.send().await` on the bounded outer).
        // If outer is closed (receiver dropped), the send fails and we exit.
        let converter_handle = tokio::spawn(async move {
            while let Some(ev) = inner_rx.recv().await {
                if outer_tx.send(ev).await.is_err() {
                    // Outer receiver dropped — converter exits.
                    break;
                }
            }
            // outer_tx drops here; recorder will see channel close and exit.
        });

        // Recorder: drain outer into the Vec.
        let snapshot_handle = tokio::spawn(async move {
            while let Some(ev) = outer_rx.recv().await {
                recorded_for_recorder.lock().await.push(ev);
            }
            // Wait for converter to fully exit so we observe POST-drain state.
            let _ = converter_handle.await;
            recorded_for_recorder.lock().await.clone()
        });

        (Self { inner_tx, outer_capacity }, snapshot_handle)
    }

    /// Send via the inner unbounded channel. Returns Err if the converter
    /// has shut down (e.g. after the outer side was dropped externally).
    pub async fn send(&self, ev: TestStreamEvent) -> Result<(), mpsc::error::SendError<TestStreamEvent>> {
        self.inner_tx.send(ev)
    }

    /// Clone the inner sender so a test can attempt a send AFTER the
    /// recorder has been dropped (proves disconnect causes a clean error).
    pub fn raw_sender(&self) -> mpsc::UnboundedSender<TestStreamEvent> {
        self.inner_tx.clone()
    }

    /// Configured outer-channel capacity (introspection only; saturation tests
    /// do not need to drain via this API — they use the snapshot handle).
    pub fn outer_capacity(&self) -> usize {
        self.outer_capacity
    }
}
