use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::agent::channel_actions::ChannelActionRouter;
use crate::agent::engine::AgentEngine;
use crate::agent::workspace::ChannelInfo;
use crate::gateway::state::ProcessingTracker;

// ── Request tracking ─────────────────────────────────────────────

/// Opaque identifier returned by [`AgentState::register_request`].
/// Used to unregister a specific request without relying on token equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestId(u64);

// ── AgentState ───────────────────────────────────────────────────

/// Mutable per-agent state shared across requests.
///
/// Extracted from `AgentEngine` so that the immutable config half
/// (`AgentConfig`, provider list, tool index, …) can live behind a
/// plain `Arc` while this struct carries the fields that change at
/// runtime.
pub struct AgentState {
    pub thinking_level: AtomicU8,
    pub channel_formatting_prompt: tokio::sync::RwLock<Option<String>>,
    pub channel_info_cache: tokio::sync::RwLock<Option<Vec<ChannelInfo>>>,
    pub processing_tracker: Option<ProcessingTracker>,
    pub channel_router: Option<ChannelActionRouter>,
    pub ui_event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    /// Tracks background tasks spawned by finalize (notifications, knowledge
    /// extraction) so graceful shutdown can wait for them to complete.
    pub bg_tasks: Arc<TaskTracker>,

    /// Weak self-reference for hot-scheduling cron jobs. Set once after Arc<AgentEngine> creation.
    pub self_ref: OnceLock<Weak<AgentEngine>>,

    /// Active request cancellation tokens — used for SIGHUP drain and shutdown.
    active_requests: Mutex<Vec<(u64, CancellationToken)>>,
    next_request_id: AtomicU64,
}

impl AgentState {
    /// Production constructor with all optional infrastructure fields.
    pub fn new(
        processing_tracker: Option<ProcessingTracker>,
        channel_router: Option<ChannelActionRouter>,
        ui_event_tx: Option<tokio::sync::broadcast::Sender<String>>,
        bg_tasks: Arc<TaskTracker>,
    ) -> Self {
        Self {
            thinking_level: AtomicU8::new(0),
            channel_formatting_prompt: tokio::sync::RwLock::new(None),
            channel_info_cache: tokio::sync::RwLock::new(None),
            processing_tracker,
            channel_router,
            ui_event_tx,
            bg_tasks,
            self_ref: OnceLock::new(),
            active_requests: Mutex::new(Vec::new()),
            next_request_id: AtomicU64::new(0),
        }
    }

    /// Store a weak self-reference after the engine is wrapped in `Arc<AgentEngine>`.
    /// Used by cron tool to hot-schedule jobs without restart.
    pub fn set_self_ref(&self, arc: &Arc<AgentEngine>) {
        let _ = self.self_ref.set(Arc::downgrade(arc));
    }

    /// Register a new in-flight request.
    ///
    /// Returns an id (for later unregistration) and a `CancellationToken`
    /// the caller should select on to detect cancellation.
    pub fn register_request(&self) -> (RequestId, CancellationToken) {
        let token = CancellationToken::new();
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.active_requests
            .lock()
            .unwrap()
            .push((id, token.clone()));
        (RequestId(id), token)
    }

    /// Cancel every active request token.
    pub fn cancel_all_requests(&self) {
        let guard = self.active_requests.lock().unwrap();
        for (_, token) in guard.iter() {
            token.cancel();
        }
    }

    /// Wait until `active_requests` is empty or `timeout` elapses.
    pub async fn wait_drain(&self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let guard = self.active_requests.lock().unwrap();
                if guard.is_empty() {
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(test)]
impl AgentState {
    /// Remove a previously registered request by its id.
    pub fn unregister_request(&self, id: &RequestId) {
        self.active_requests
            .lock()
            .unwrap()
            .retain(|(i, _)| *i != id.0);
    }

    /// Number of currently tracked active requests.
    pub fn active_request_count(&self) -> usize {
        self.active_requests.lock().unwrap().len()
    }

    /// Test-only constructor — all optional fields `None`/default/empty.
    pub fn test_new() -> Arc<Self> {
        Arc::new(Self {
            thinking_level: AtomicU8::new(0),
            channel_formatting_prompt: tokio::sync::RwLock::new(None),
            channel_info_cache: tokio::sync::RwLock::new(None),
            processing_tracker: None,
            channel_router: None,
            ui_event_tx: None,
            bg_tasks: Arc::new(TaskTracker::new()),
            self_ref: OnceLock::new(),
            active_requests: Mutex::new(Vec::new()),
            next_request_id: AtomicU64::new(0),
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_state_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentState>();
    }

    #[test]
    fn test_agent_state_test_new_defaults() {
        let state = AgentState::test_new();
        assert_eq!(state.thinking_level.load(Ordering::Relaxed), 0);
        assert!(state.processing_tracker.is_none());
        assert!(state.channel_router.is_none());
        assert!(state.ui_event_tx.is_none());
        assert_eq!(state.active_request_count(), 0);
        assert!(state.bg_tasks.is_empty());
    }

    #[tokio::test]
    async fn test_register_and_cancel_request() {
        let state = AgentState::test_new();
        let (_id, token) = state.register_request();
        assert_eq!(state.active_request_count(), 1);
        assert!(!token.is_cancelled());

        state.cancel_all_requests();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_unregister_request_removes_token() {
        let state = AgentState::test_new();
        let (id, _token) = state.register_request();
        assert_eq!(state.active_request_count(), 1);

        state.unregister_request(&id);
        assert_eq!(state.active_request_count(), 0);
    }

    #[tokio::test]
    async fn test_wait_drain_returns_when_empty() {
        let state = AgentState::test_new();
        // No active requests — should return nearly instantly.
        let start = tokio::time::Instant::now();
        state.wait_drain(Duration::from_secs(5)).await;
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn test_wait_drain_returns_after_unregister() {
        let state = AgentState::test_new();
        let (id, _token) = state.register_request();

        let state2 = state.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            state2.unregister_request(&id);
        });

        state.wait_drain(Duration::from_secs(5)).await;
        assert_eq!(state.active_request_count(), 0);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_wait_drain_respects_timeout() {
        let state = AgentState::test_new();
        let (_id, _token) = state.register_request();

        let start = tokio::time::Instant::now();
        state.wait_drain(Duration::from_millis(200)).await;
        let elapsed = start.elapsed();
        // Should have timed out — request never removed
        assert!(elapsed >= Duration::from_millis(150));
        assert_eq!(state.active_request_count(), 1);
    }
}
