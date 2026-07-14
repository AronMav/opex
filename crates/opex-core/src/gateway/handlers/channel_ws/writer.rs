//! Single-writer task. Owns the WS sink, drains an `mpsc<OutboundMsg>` and
//! serialises every `ChannelOutbound` (or raw control frame) to a WS frame.
//! Exits on [`OutboundMsg::Shutdown`], `mpsc` close, or sink error.
//!
//! Centralising sink ownership eliminates the need for `Arc<Mutex<SplitSink>>`
//! across the dispatcher / inline handlers / engine-action forwarding.

use axum::extract::ws::Message as WsMessage;
use futures_util::{Sink, SinkExt};
use tokio::sync::mpsc;

use super::types::OutboundMsg;
use super::ws_json;

/// Max time a single WS write may take before the writer gives up and exits,
/// tearing down the connection. Chosen above the 30s app-level ping and the
/// 20s tool-action grace so a healthy-but-idle adapter is never killed (#4).
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Drain `rx` and write every message to `sink`. Returns when `rx` closes,
/// `Shutdown` arrives, or the sink errors.
///
/// Generic over `S` (any `Sink<WsMessage>` with a `Display` error) so unit
/// tests can substitute an in-memory `futures::channel::mpsc::UnboundedSender`
/// in place of an Axum WS sink.
pub(super) async fn run<S>(sink: S, rx: mpsc::Receiver<OutboundMsg>)
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    run_with_timeout(sink, rx, WRITE_TIMEOUT).await
}

/// Internal: drain loop with injected timeout (for testing with short durations).
async fn run_with_timeout<S>(mut sink: S, mut rx: mpsc::Receiver<OutboundMsg>, write_timeout: std::time::Duration)
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    while let Some(msg) = rx.recv().await {
        match msg {
            OutboundMsg::Wire(payload) => {
                match tokio::time::timeout(write_timeout, sink.send(ws_json(&payload))).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "channel WS writer: sink send failed, exiting");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!("channel WS writer: send timed out ({write_timeout:?}), exiting — adapter stuck");
                        return;
                    }
                }
            }
            OutboundMsg::Ping => {
                match tokio::time::timeout(
                    write_timeout,
                    sink.send(WsMessage::Ping(vec![1, 2, 3, 4].into())),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "channel WS writer: ping send failed, exiting");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!("channel WS writer: ping timed out ({write_timeout:?}), exiting — adapter stuck");
                        return;
                    }
                }
            }
            OutboundMsg::Shutdown => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::ChannelOutbound;
    use std::sync::{Arc, Mutex};

    /// Capture-sink: thin `Sink<WsMessage>` impl pushing every frame into a
    /// shared `Vec` so tests can inspect order and content. Always Ok — never
    /// pretends to fail since the writer's error paths are exercised by
    /// `writer_exits_when_sender_dropped` (mpsc close → recv returns None).
    struct CaptureSink {
        captured: Arc<Mutex<Vec<WsMessage>>>,
    }

    impl Sink<WsMessage> for CaptureSink {
        type Error = std::convert::Infallible;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn start_send(self: std::pin::Pin<&mut Self>, item: WsMessage) -> Result<(), Self::Error> {
            self.captured.lock().unwrap().push(item);
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    // Note: `std::convert::Infallible` already implements `Display` in std,
    // satisfying the writer's `S::Error: Display` bound.

    /// A sink whose `poll_ready` never resolves — simulates a stuck-but-open
    /// adapter. `tokio::time::timeout`'s own timer fires regardless of whether
    /// this registers a waker, so the writer still exits.
    struct StuckSink;

    impl Sink<WsMessage> for StuckSink {
        type Error = std::convert::Infallible;
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }
        fn start_send(self: std::pin::Pin<&mut Self>, _item: WsMessage) -> Result<(), Self::Error> {
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn writer_exits_on_stuck_sink_after_timeout() {
        // Real (not paused) time + a short injected timeout → deterministic:
        // the stuck StuckSink never completes a send, so run_with_timeout must
        // hit the write timeout and exit within a generous window.
        let (tx, rx) = mpsc::channel::<OutboundMsg>(4);
        let short = std::time::Duration::from_millis(50);
        let h = tokio::spawn(run_with_timeout(StuckSink, rx, short));
        tx.send(OutboundMsg::Wire(ChannelOutbound::Chunk {
            request_id: "r".to_string(),
            text: "stuck".to_string(),
        }))
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), h)
            .await
            .expect("writer must exit after its write timeout")
            .unwrap();
    }

    #[tokio::test]
    async fn writer_serialises_in_order_then_exits_on_shutdown() {
        let captured = Arc::new(Mutex::new(Vec::<WsMessage>::new()));
        let sink = CaptureSink { captured: captured.clone() };
        let (tx, rx) = mpsc::channel::<OutboundMsg>(8);
        let h = tokio::spawn(run(sink, rx));

        for i in 0..3 {
            tx.send(OutboundMsg::Wire(ChannelOutbound::Chunk {
                request_id: format!("r{i}"),
                text: format!("hello-{i}"),
            }))
            .await
            .unwrap();
        }
        tx.send(OutboundMsg::Shutdown).await.unwrap();
        drop(tx);
        h.await.unwrap();

        let frames = captured.lock().unwrap();
        assert_eq!(frames.len(), 3, "expected 3 frames before shutdown");
        for (i, frame) in frames.iter().enumerate() {
            let WsMessage::Text(t) = frame else { panic!("expected Text frame") };
            assert!(t.contains(&format!("hello-{i}")), "frame {i} content");
            assert!(t.contains(&format!("r{i}")), "frame {i} request_id");
        }
    }

    #[tokio::test]
    async fn writer_emits_raw_ping_for_ping_variant() {
        let captured = Arc::new(Mutex::new(Vec::<WsMessage>::new()));
        let sink = CaptureSink { captured: captured.clone() };
        let (tx, rx) = mpsc::channel::<OutboundMsg>(4);
        let h = tokio::spawn(run(sink, rx));

        tx.send(OutboundMsg::Ping).await.unwrap();
        tx.send(OutboundMsg::Shutdown).await.unwrap();
        drop(tx);
        h.await.unwrap();

        let frames = captured.lock().unwrap();
        assert!(matches!(frames.first(), Some(WsMessage::Ping(_))),
            "Ping variant must produce raw WS Ping frame");
    }

    #[tokio::test]
    async fn writer_exits_when_sender_dropped() {
        let captured = Arc::new(Mutex::new(Vec::<WsMessage>::new()));
        let sink = CaptureSink { captured };
        let (tx, rx) = mpsc::channel::<OutboundMsg>(2);
        let h = tokio::spawn(run(sink, rx));
        drop(tx);
        h.await.unwrap();
    }
}
