//! 16 ms text-delta coalescer — preserves order, drops only under saturation.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};

use crate::agent::engine::StreamEvent;
use crate::metrics::MetricsRegistry;

/// Coalescing window for TextDelta events. Other events bypass the window.
pub const COALESCE_WINDOW_MS: u64 = 16;

/// Map a StreamEvent to a short label for the metrics counter.
/// KEEP IN SYNC with the SSE event-type table in CLAUDE.md.
fn event_type_label(ev: &StreamEvent) -> &'static str {
    match ev {
        StreamEvent::SessionId(_) => "session-id",
        StreamEvent::MessageStart { .. } => "message-start",
        StreamEvent::StepStart { .. } => "step-start",
        StreamEvent::TextDelta(_) => "text-delta",
        StreamEvent::ToolCallStart { .. } => "tool-call-start",
        StreamEvent::ToolCallArgs { .. } => "tool-call-args",
        StreamEvent::ToolResult { .. } => "tool-result",
        StreamEvent::StepFinish { .. } => "step-finish",
        StreamEvent::RichCard { .. } => "rich-card",
        StreamEvent::File { .. } => "file",
        StreamEvent::AgentSwitch { .. } => "agent-switch",
        StreamEvent::ApprovalNeeded { .. } => "approval-needed",
        StreamEvent::ApprovalResolved { .. } => "approval-resolved",
        StreamEvent::Finish { .. } => "finish",
        StreamEvent::Error(_) => "error",
        StreamEvent::Reconnecting { .. } => "reconnecting",
    }
}

/// Spawn the coalescer task. Reads from `raw_rx`, writes to `outer_tx`.
///
/// * `raw_rx` — engine-side receiver (bounded 256 in chat.rs).
/// * `outer_tx` — converter-side unbounded sender. The coalescer is the sole
///   producer and is rate-limited by the 16 ms tick, so unbounded here is safe.
/// * `metrics` — shared Arc<MetricsRegistry>. Drops are recorded here.
/// * `agent_label` — agent name for the drop-counter label.
pub fn spawn_coalescing_converter(
    mut raw_rx: mpsc::Receiver<StreamEvent>,
    outer_tx: mpsc::UnboundedSender<StreamEvent>,
    metrics: Arc<MetricsRegistry>,
    agent_label: String,
) {
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_millis(COALESCE_WINDOW_MS));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut pending_text = String::new();

        loop {
            tokio::select! {
                maybe_ev = raw_rx.recv() => {
                    let Some(ev) = maybe_ev else { break };
                    match ev {
                        StreamEvent::TextDelta(s) => {
                            pending_text.push_str(&s);
                        }
                        other => {
                            // Flush pending text FIRST to preserve order.
                            if !pending_text.is_empty() {
                                let flushed = std::mem::take(&mut pending_text);
                                if outer_tx.send(StreamEvent::TextDelta(flushed)).is_err() {
                                    metrics.record_sse_drop(&agent_label, "text-delta");
                                }
                            }
                            // Non-text events NEVER dropped silently. Send through
                            // the unbounded outer channel — which never blocks.
                            // A drop here only happens if the receiver was dropped.
                            let label = event_type_label(&other);
                            if outer_tx.send(other).is_err() {
                                metrics.record_sse_drop(&agent_label, label);
                            }
                        }
                    }
                }
                _ = tick.tick() => {
                    if !pending_text.is_empty() {
                        let flushed = std::mem::take(&mut pending_text);
                        if outer_tx.send(StreamEvent::TextDelta(flushed)).is_err() {
                            metrics.record_sse_drop(&agent_label, "text-delta");
                        }
                    }
                }
            }
        }

        // Drain: flush residual text after raw_rx closes.
        if !pending_text.is_empty() {
            let flushed = std::mem::take(&mut pending_text);
            if outer_tx.send(StreamEvent::TextDelta(flushed)).is_err() {
                metrics.record_sse_drop(&agent_label, "text-delta");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MetricsRegistry;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coalesces_three_textdeltas_into_one_flush() {
        let (raw_tx, raw_rx) = mpsc::channel::<StreamEvent>(256);
        let (outer_tx, mut outer_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let metrics = Arc::new(MetricsRegistry::new());

        spawn_coalescing_converter(raw_rx, outer_tx, metrics.clone(), "agent-a".to_string());

        raw_tx.send(StreamEvent::TextDelta("hello ".into())).await.unwrap();
        raw_tx.send(StreamEvent::TextDelta("world".into())).await.unwrap();
        raw_tx.send(StreamEvent::TextDelta("!".into())).await.unwrap();

        // Wait for 2 ticks (~32 ms) so the coalescer flushes.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(raw_tx);

        let mut collected = Vec::new();
        while let Some(ev) = outer_rx.recv().await {
            collected.push(ev);
        }

        let text_events: Vec<&String> = collected
            .iter()
            .filter_map(|e| if let StreamEvent::TextDelta(s) = e { Some(s) } else { None })
            .collect();

        assert!(
            !text_events.is_empty() && text_events.len() <= 3,
            "expected 1-3 merged text events, got {}: {:?}",
            text_events.len(),
            collected
        );
        let merged: String = text_events.into_iter().cloned().collect();
        assert_eq!(
            merged, "hello world!",
            "merged text must preserve order; got {merged:?} (full collected: {collected:?})"
        );
        assert!(
            metrics.snapshot_sse_drops().is_empty(),
            "no drops expected under normal conditions"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nontext_event_flushes_pending_text_first() {
        let (raw_tx, raw_rx) = mpsc::channel::<StreamEvent>(256);
        let (outer_tx, mut outer_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let metrics = Arc::new(MetricsRegistry::new());

        spawn_coalescing_converter(raw_rx, outer_tx, metrics.clone(), "agent-a".to_string());

        raw_tx.send(StreamEvent::TextDelta("abc".into())).await.unwrap();
        raw_tx.send(StreamEvent::TextDelta("def".into())).await.unwrap();
        raw_tx
            .send(StreamEvent::ToolCallStart { id: "t1".into(), name: "tool".into() })
            .await
            .unwrap();
        drop(raw_tx);

        let mut collected = Vec::new();
        while let Some(ev) = outer_rx.recv().await {
            collected.push(ev);
        }

        // First event: merged TextDelta "abcdef". Second: ToolCallStart.
        assert!(
            matches!(collected[0], StreamEvent::TextDelta(ref s) if s == "abcdef"),
            "first event must be merged TextDelta('abcdef'); got: {:?}",
            collected
        );
        assert!(
            matches!(collected[1], StreamEvent::ToolCallStart { .. }),
            "second event must be ToolCallStart; got: {:?}",
            collected
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_on_close_flushes_residual_text() {
        let (raw_tx, raw_rx) = mpsc::channel::<StreamEvent>(256);
        let (outer_tx, mut outer_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let metrics = Arc::new(MetricsRegistry::new());

        spawn_coalescing_converter(raw_rx, outer_tx, metrics.clone(), "agent-a".to_string());

        raw_tx.send(StreamEvent::TextDelta("x".into())).await.unwrap();
        drop(raw_tx);

        timeout(Duration::from_secs(2), async {
            let ev = outer_rx.recv().await.expect("residual text flushed");
            assert!(
                matches!(ev, StreamEvent::TextDelta(ref s) if s == "x"),
                "residual 'x' must be flushed on close; got: {ev:?}"
            );
            assert!(outer_rx.recv().await.is_none(), "channel must close after residual");
        })
        .await
        .expect("drain timeout");
    }
}
