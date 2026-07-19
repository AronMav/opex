//! Transport-agnostic event sink for pipeline::execute.
//!
//! PipelineEvent = StreamEvent (web SSE events) | ProcessingPhase (channel typing).
//! Each sink chooses which variants to forward and silently drops the rest.

use crate::agent::engine::stream::ProcessingPhase;
use crate::agent::stream_event::StreamEvent;

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    Stream(StreamEvent),
    Phase(ProcessingPhase),
}

impl From<StreamEvent> for PipelineEvent {
    fn from(ev: StreamEvent) -> Self {
        PipelineEvent::Stream(ev)
    }
}
impl From<ProcessingPhase> for PipelineEvent {
    fn from(p: ProcessingPhase) -> Self {
        PipelineEvent::Phase(p)
    }
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Full/Fatal used once sinks wire backpressure/infra errors (Tasks 5+).
pub enum SinkError {
    #[error("sink closed (client disconnected)")]
    Closed,
    #[error("sink full (backpressure)")]
    Full,
    #[error(transparent)]
    Fatal(#[from] anyhow::Error),
}

pub trait EventSink: Send {
    async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError>;
    #[allow(dead_code)] // default close() is overridden by collector-style sinks in later tasks.
    async fn close(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    #[derive(Default, Debug)]
    pub struct MockSink {
        pub events: Vec<PipelineEvent>,
        pub closed_after: Option<usize>,
    }

    impl MockSink {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn close_after(n: usize) -> Self {
            Self { closed_after: Some(n), ..Self::default() }
        }
    }

    impl EventSink for MockSink {
        async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError> {
            if let Some(n) = self.closed_after
                && self.events.len() >= n
            {
                return Err(SinkError::Closed);
            }
            self.events.push(ev);
            Ok(())
        }
    }
}

// ── Production sinks ──────────────────────────────────────────────────

use crate::agent::engine_event_sender::EngineEventSender;

pub struct SseSink { tx: EngineEventSender }

impl SseSink {
    pub fn new(tx: EngineEventSender) -> Self { Self { tx } }
}

impl EventSink for SseSink {
    async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError> {
        match ev {
            // H10 fix: distinguish DroppedTextDelta (a contract-permitted
            // transient drop under coalescer saturation) from a true Closed
            // signal. The old mapping conflated both into SinkError::Closed,
            // which execute() then interpreted as "client disconnected" and
            // terminated the turn as Interrupted on a false positive.
            PipelineEvent::Stream(se) => match self.tx.send_async(se).await {
                Ok(()) => Ok(()),
                Err(crate::agent::engine_event_sender::EngineSendError::DroppedTextDelta) => Ok(()),
                Err(_) => Err(SinkError::Closed),
            },
            PipelineEvent::Phase(_)   => Ok(()), // SSE does not transport typing indicator
        }
    }
}

pub struct ChannelStatusSink {
    status_tx: Option<tokio::sync::mpsc::UnboundedSender<ProcessingPhase>>,
    chunk_tx:  Option<tokio::sync::mpsc::Sender<String>>,
    pub buffer: String,
    /// Captured `handler_menu` rich-card data (if the turn emitted one). After
    /// the pipeline, `handle_with_status` turns this into a `send_menu` channel
    /// action so channels (Telegram) can render clickable buttons.
    pub menu: Option<serde_json::Value>,
}

impl ChannelStatusSink {
    pub fn new(
        status_tx: Option<tokio::sync::mpsc::UnboundedSender<ProcessingPhase>>,
        chunk_tx:  Option<tokio::sync::mpsc::Sender<String>>,
    ) -> Self { Self { status_tx, chunk_tx, buffer: String::new(), menu: None } }
}

impl EventSink for ChannelStatusSink {
    async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError> {
        match ev {
            PipelineEvent::Phase(p) => {
                // H9 fix: surface a closed status channel as SinkError::Closed
                // so execute() can transition the turn to Interrupted instead
                // of silently swallowing the error and continuing to emit into
                // the void. `UnboundedSender::send` only errs on a closed
                // receiver, so the mapping is unambiguous.
                if let Some(tx) = &self.status_tx
                    && tx.send(p).is_err()
                {
                    return Err(SinkError::Closed);
                }
                Ok(())
            }
            PipelineEvent::Stream(StreamEvent::TextDelta(s)) => {
                self.buffer.push_str(&s);
                if let Some(tx) = &self.chunk_tx {
                    tx.send(s).await.map_err(|_| SinkError::Closed)
                } else { Ok(()) }
            }
            PipelineEvent::Stream(StreamEvent::RichCard { card_type, data }) => {
                // Capture the handler-selection menu (file handlers) or a
                // slash-command args menu for channel rendering.
                if card_type == "handler_menu" || card_type == "command_args_menu" {
                    self.menu = Some(data);
                }
                Ok(())
            }
            _ => Ok(()), // other tool/file events not relevant to channel transport
        }
    }
}

pub struct ChunkSink {
    chunk_tx: tokio::sync::mpsc::Sender<String>,
    pub buffer: String,
}

impl ChunkSink {
    pub fn new(chunk_tx: tokio::sync::mpsc::Sender<String>) -> Self {
        Self { chunk_tx, buffer: String::new() }
    }
}

impl EventSink for ChunkSink {
    async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError> {
        if let PipelineEvent::Stream(StreamEvent::TextDelta(s)) = ev {
            self.buffer.push_str(&s);
            self.chunk_tx.send(s).await.map_err(|_| SinkError::Closed)
        } else { Ok(()) }
    }
}

/// Sink that drops every event. Used by RPC-style callers (cron jobs,
/// agent-to-agent messaging) that consume only the final assistant text
/// from `ExecuteOutcome`/`finalize` and have no use for the per-chunk
/// streaming events.
///
/// Does buffer text deltas so callers that want the final message
/// without going through the DB can read it from `buffer` after
/// `pipeline::execute` returns. (`finalize` writes the same text to
/// the DB regardless.)
pub struct NoopSink {
    pub buffer: String,
}

impl Default for NoopSink {
    fn default() -> Self {
        Self::new()
    }
}

impl NoopSink {
    pub fn new() -> Self {
        Self { buffer: String::new() }
    }
}

impl EventSink for NoopSink {
    async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError> {
        if let PipelineEvent::Stream(StreamEvent::TextDelta(s)) = ev {
            self.buffer.push_str(&s);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_support::MockSink;

    #[tokio::test]
    async fn mock_sink_records_events() {
        let mut sink = MockSink::new();
        sink.emit(StreamEvent::TextDelta("a".into()).into()).await.unwrap();
        sink.emit(ProcessingPhase::Thinking.into()).await.unwrap();
        assert_eq!(sink.events.len(), 2);
    }

    #[tokio::test]
    async fn mock_sink_closes_after_limit() {
        let mut sink = MockSink::close_after(1);
        sink.emit(StreamEvent::TextDelta("ok".into()).into()).await.unwrap();
        let err = sink.emit(StreamEvent::TextDelta("drop".into()).into()).await;
        assert!(matches!(err, Err(SinkError::Closed)));
    }

    #[tokio::test]
    async fn sse_sink_forwards_stream_events() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(8);
        let mut sink = SseSink::new(EngineEventSender::new(tx));
        sink.emit(StreamEvent::TextDelta("hi".into()).into()).await.unwrap();
        assert!(matches!(rx.recv().await, Some(StreamEvent::TextDelta(ref s)) if s == "hi"));
    }

    #[tokio::test]
    async fn sse_sink_drops_phase() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(8);
        let mut sink = SseSink::new(EngineEventSender::new(tx));
        sink.emit(ProcessingPhase::Thinking.into()).await.unwrap();
        drop(sink);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn sse_sink_returns_closed_on_drop() {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(8);
        let mut sink = SseSink::new(EngineEventSender::new(tx));
        drop(rx);
        let err = sink.emit(StreamEvent::TextDelta("x".into()).into()).await;
        assert!(matches!(err, Err(SinkError::Closed)));
    }

    #[tokio::test]
    async fn channel_status_sink_routes_phase_to_status() {
        let (st, mut st_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ch, _ch_rx)    = tokio::sync::mpsc::channel(8);
        let mut sink = ChannelStatusSink::new(Some(st), Some(ch));
        sink.emit(ProcessingPhase::Thinking.into()).await.unwrap();
        assert!(matches!(st_rx.recv().await, Some(ProcessingPhase::Thinking)));
    }

    #[tokio::test]
    async fn channel_status_sink_routes_text_to_chunks_and_buffers() {
        let (ch, mut ch_rx) = tokio::sync::mpsc::channel(8);
        let mut sink = ChannelStatusSink::new(None, Some(ch));
        sink.emit(StreamEvent::TextDelta("hello".into()).into()).await.unwrap();
        assert_eq!(ch_rx.recv().await, Some("hello".into()));
        assert_eq!(sink.buffer, "hello");
    }

    #[tokio::test]
    async fn channel_status_sink_drops_tool_events() {
        let (ch, mut ch_rx) = tokio::sync::mpsc::channel(8);
        let mut sink = ChannelStatusSink::new(None, Some(ch));
        sink.emit(StreamEvent::MessageStart { message_id: opex_types::ids::MessageId::from(uuid::Uuid::nil()) }.into()).await.unwrap();
        drop(sink);
        assert!(ch_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn chunk_sink_emits_only_text_deltas() {
        let (ch, mut ch_rx) = tokio::sync::mpsc::channel(8);
        let mut sink = ChunkSink::new(ch);
        sink.emit(StreamEvent::TextDelta("abc".into()).into()).await.unwrap();
        sink.emit(StreamEvent::MessageStart { message_id: opex_types::ids::MessageId::from(uuid::Uuid::nil()) }.into()).await.unwrap();
        assert_eq!(ch_rx.recv().await, Some("abc".into()));
        drop(sink);
        assert!(ch_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn chunk_sink_returns_closed_when_receiver_dropped() {
        let (ch, rx) = tokio::sync::mpsc::channel(8);
        let mut sink = ChunkSink::new(ch);
        drop(rx);
        let err = sink.emit(StreamEvent::TextDelta("x".into()).into()).await;
        assert!(matches!(err, Err(SinkError::Closed)));
    }

    #[tokio::test]
    async fn channel_sink_captures_command_args_menu() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut sink = ChannelStatusSink::new(None, Some(tx));
        let card = serde_json::json!({"card_type":"command_args_menu","x":1});
        sink.emit(PipelineEvent::Stream(StreamEvent::RichCard {
            card_type: "command_args_menu".into(), data: card.clone() })).await.unwrap();
        assert_eq!(sink.menu, Some(card));
    }
}
