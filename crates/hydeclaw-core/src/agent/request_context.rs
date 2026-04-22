#![allow(dead_code)] // Scaffolding — wired in by later tasks.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::engine::StreamEvent;
use crate::agent::tool_loop::{LoopDetector, ToolLoopConfig};
use crate::db::session_wal;

/// Per-request execution context, holding session identity, cancellation,
/// loop detection, and the optional SSE sender.
pub struct RequestContext {
    pub session_id: Uuid,
    pub message_id: String,
    pub cancel: CancellationToken,
    pub loop_detector: Arc<Mutex<LoopDetector>>,
    pub sse_tx: Option<mpsc::UnboundedSender<StreamEvent>>,
    pub leaf_message_id: Option<String>,
}

impl RequestContext {
    /// Create a fresh context (no WAL warm-up).
    pub fn new(session_id: Uuid, cancel: CancellationToken, loop_config: &ToolLoopConfig) -> Self {
        Self {
            session_id,
            message_id: Uuid::new_v4().to_string(),
            cancel,
            loop_detector: Arc::new(Mutex::new(LoopDetector::new(loop_config))),
            sse_tx: None,
            leaf_message_id: None,
        }
    }

    /// Create a context with WAL warm-up — replays tool_end events into LoopDetector.
    /// Best-effort: if WAL read fails, proceeds with a fresh detector.
    pub async fn new_for_session(
        db: &sqlx::PgPool,
        session_id: Uuid,
        cancel: CancellationToken,
        loop_config: &ToolLoopConfig,
    ) -> Self {
        let detector = match session_wal::load_tool_events(db, session_id).await {
            Ok(events) => {
                if !events.is_empty() {
                    tracing::debug!(
                        session_id = %session_id,
                        count = events.len(),
                        "WAL warm-up: replayed tool events into LoopDetector",
                    );
                }
                LoopDetector::warm_up_from_wal(loop_config, &events)
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "WAL warm-up failed, proceeding with fresh LoopDetector",
                );
                LoopDetector::new(loop_config)
            }
        };

        Self {
            session_id,
            message_id: Uuid::new_v4().to_string(),
            cancel,
            loop_detector: Arc::new(Mutex::new(detector)),
            sse_tx: None,
            leaf_message_id: None,
        }
    }

    /// Emit an SSE event. No-op if `sse_tx` is `None` or the channel is closed.
    pub fn emit(&self, event: StreamEvent) {
        if let Some(tx) = &self.sse_tx {
            let _ = tx.send(event);
        }
    }

    /// Create a minimal context for unit tests (no DB, no SSE, dummy IDs).
    pub fn test_new() -> Self {
        Self {
            session_id: Uuid::nil(),
            message_id: "test-message".to_string(),
            cancel: CancellationToken::new(),
            loop_detector: Arc::new(Mutex::new(LoopDetector::new(&ToolLoopConfig::default()))),
            sse_tx: None,
            leaf_message_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn test_request_context_is_send() {
        assert_send::<RequestContext>();
    }

    #[test]
    fn test_request_context_test_new() {
        let ctx = RequestContext::test_new();
        assert_eq!(ctx.session_id, Uuid::nil());
        assert_eq!(ctx.message_id, "test-message");
        assert!(ctx.sse_tx.is_none());
        assert!(ctx.leaf_message_id.is_none());
        assert!(!ctx.cancel.is_cancelled());
    }

    #[test]
    fn test_cancellation_propagates() {
        let ctx = RequestContext::test_new();
        let child = ctx.cancel.child_token();
        assert!(!child.is_cancelled());
        ctx.cancel.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn test_emit_noop_without_tx() {
        let ctx = RequestContext::test_new();
        // Should not panic even though sse_tx is None.
        ctx.emit(StreamEvent::TextDelta("hello".into()));
    }

    #[tokio::test]
    async fn test_emit_sends_to_channel() {
        let mut ctx = RequestContext::test_new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        ctx.sse_tx = Some(tx);

        ctx.emit(StreamEvent::TextDelta("hello".into()));

        let received = rx.recv().await.expect("should receive event");
        match received {
            StreamEvent::TextDelta(text) => assert_eq!(text, "hello"),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
