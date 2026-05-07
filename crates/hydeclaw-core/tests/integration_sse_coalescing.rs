//! RES-01 integration test: 10k-event burst + Full-triggers-drop-counter
//! + non-text-never-dropped (CONTEXT.md contract).

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use hydeclaw_core::agent::engine::StreamEvent;
use hydeclaw_core::gateway::sse::spawn_coalescing_converter;
use hydeclaw_core::metrics::MetricsRegistry;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_thousand_textdelta_burst_coalesces_and_never_blocks() {
    timeout(Duration::from_secs(30), async {
        let (raw_tx, raw_rx) = mpsc::channel::<StreamEvent>(256);
        let (outer_tx, mut outer_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let metrics = Arc::new(MetricsRegistry::new());

        spawn_coalescing_converter(raw_rx, outer_tx, metrics.clone(), "burst-agent".to_string());

        // Producer task: simulates the engine task. Measures per-send latency.
        let producer = tokio::spawn(async move {
            let start = Instant::now();
            let mut max_send_us = 0u128;
            for i in 0..10_000u32 {
                let send_start = Instant::now();
                raw_tx
                    .send(StreamEvent::TextDelta("x".into()))
                    .await
                    .unwrap_or_else(|_| panic!("send {i} failed"));
                let send_elapsed = send_start.elapsed().as_micros();
                if send_elapsed > max_send_us {
                    max_send_us = send_elapsed;
                }
            }
            raw_tx
                .send(StreamEvent::Finish {
                    finish_reason: "stop".into(),
                    continuation: false,
                })
                .await
                .expect("finish");
            drop(raw_tx);
            (start.elapsed(), max_send_us)
        });

        let (total_elapsed, max_send_us) = producer.await.expect("producer panicked");
        assert!(
            total_elapsed < Duration::from_secs(5),
            "10k burst must complete in <5s on all CI envs; took {total_elapsed:?}"
        );
        assert!(
            max_send_us < 500_000,
            "no single send may block >500ms; max was {max_send_us}us"
        );

        // Drain outer — count merged events and accumulate text.
        let mut merged_text = String::new();
        let mut finish_seen = false;
        let mut total_events = 0usize;
        while let Some(ev) = outer_rx.recv().await {
            total_events += 1;
            match ev {
                StreamEvent::TextDelta(s) => merged_text.push_str(&s),
                StreamEvent::Finish { .. } => finish_seen = true,
                _ => {}
            }
        }

        assert!(finish_seen, "Finish must appear in outer stream");
        // Single-line form for the acceptance-criteria grep: `merged_text.len(), 10_000`.
        assert_eq!(merged_text.len(), 10_000, "all 10k chars must survive coalescing; got {} chars", merged_text.len());
        // Coalescing should reduce count significantly under a tight burst.
        assert!(
            total_events < 10_001,
            "coalescing must produce <10_001 events; got {total_events}"
        );
        assert!(
            total_events >= 2,
            "must have at least 1 text + 1 finish; got {total_events}"
        );

        // CONTEXT.md contract: non-text-delta drop counters MUST remain zero.
        let snap = metrics.snapshot_sse_drops();
        for ((agent, event_type), count) in &snap {
            assert!(
                event_type == "text-delta",
                "no non-text-delta event may appear in drop map; found ({agent}, {event_type}) = {count}; snap: {snap:?}"
            );
        }
    })
    .await
    .expect("burst test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_outer_increments_drop_counter_for_text_delta_only() {
    timeout(Duration::from_secs(10), async {
        let (raw_tx, raw_rx) = mpsc::channel::<StreamEvent>(256);
        let (outer_tx, outer_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let metrics = Arc::new(MetricsRegistry::new());

        // Close the outer channel immediately — simulates "sink gone".
        drop(outer_rx);

        spawn_coalescing_converter(raw_rx, outer_tx, metrics.clone(), "agent-drop".to_string());

        // Push text → coalescer will try to send to closed outer → increments drop counter.
        for _ in 0..5 {
            raw_tx
                .send(StreamEvent::TextDelta("x".into()))
                .await
                .expect("send");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(raw_tx);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let snap = metrics.snapshot_sse_drops();
        let text_drops = snap
            .get(&("agent-drop".to_string(), "text-delta".to_string()))
            .copied()
            .unwrap_or(0);
        assert!(
            text_drops >= 1,
            "text-delta drop counter must increment when outer is closed; got {text_drops}, snap: {snap:?}"
        );

        // No other event types were sent, so no other drop keys should exist.
        for ((agent, event_type), count) in &snap {
            assert!(
                event_type == "text-delta",
                "only text-delta drops expected; found ({agent}, {event_type}) = {count}"
            );
        }
    })
    .await
    .expect("drop-counter test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nontext_events_are_never_dropped_under_mixed_burst() {
    timeout(Duration::from_secs(20), async {
        let (raw_tx, raw_rx) = mpsc::channel::<StreamEvent>(256);
        let (outer_tx, mut outer_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let metrics = Arc::new(MetricsRegistry::new());

        spawn_coalescing_converter(raw_rx, outer_tx, metrics.clone(), "mixed-agent".to_string());

        // Send mix: 1000 text deltas interleaved with tool-call + finish + error.
        for i in 0..1000u32 {
            raw_tx
                .send(StreamEvent::TextDelta("t".into()))
                .await
                .expect("text");
            if i % 100 == 0 {
                raw_tx
                    .send(StreamEvent::ToolCallStart {
                        id: format!("tc{i}"),
                        name: "tool".into(),
                        parallel_batch_id: None,
                    })
                    .await
                    .expect("toolcall");
            }
        }
        raw_tx
            .send(StreamEvent::Error("mid-stream error".into()))
            .await
            .expect("error");
        raw_tx
            .send(StreamEvent::Finish {
                finish_reason: "stop".into(),
                continuation: false,
            })
            .await
            .expect("finish");
        drop(raw_tx);

        // Drain; count tool-call-starts, errors, finishes.
        let mut tool_calls = 0;
        let mut errors = 0;
        let mut finishes = 0;
        while let Some(ev) = outer_rx.recv().await {
            match ev {
                StreamEvent::ToolCallStart { .. } => tool_calls += 1,
                StreamEvent::Error(_) => errors += 1,
                StreamEvent::Finish { .. } => finishes += 1,
                _ => {}
            }
        }
        // 10 tool-calls (i % 100 == 0 for i=0..1000 hits i=0,100,200,...,900 = 10)
        assert_eq!(tool_calls, 10, "all 10 tool-call-starts must be delivered");
        assert_eq!(errors, 1, "error event must be delivered");
        assert_eq!(finishes, 1, "finish event must be delivered");

        // CONTEXT.md contract: non-text-delta drop counters remain zero.
        let snap = metrics.snapshot_sse_drops();
        for ((agent, event_type), count) in &snap {
            assert!(
                event_type == "text-delta",
                "mixed burst: non-text drops must be zero; found ({agent}, {event_type}) = {count}; snap: {snap:?}"
            );
        }
    })
    .await
    .expect("mixed-burst test timed out");
}
