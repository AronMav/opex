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

/// RAII guard for an in-flight request registration.
///
/// R-DRAIN fix: previously call sites did `let _cancel_guard =
/// state.register_request();` but `unregister_request` was `#[cfg(test)]`-only,
/// so in production `active_requests` grew unbounded (a slow leak) AND
/// `wait_drain` never saw it empty — every graceful shutdown blocked the FULL
/// drain timeout. This guard removes the entry on drop (normal completion, `?`
/// early-return, panic), so the registry reflects reality and drain returns as
/// soon as live requests finish. It also registers the caller's REAL pipeline
/// cancel token (see `register_request_guarded`), so `cancel_all_requests`
/// actually propagates into `execute()` instead of cancelling an orphan token.
pub struct RequestGuard {
    state: Arc<AgentState>,
    id: RequestId,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.state.remove_request(&self.id);
    }
}

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
    /// T17 triage: last computed estimate-only context breakdown (per
    /// category system_prompt/tools/conversation/... sizes). Refreshed on
    /// every `build_context` call in bootstrap; read by
    /// `GET /api/agents/{name}/context-breakdown`. `None` until the agent's
    /// first turn since process start.
    pub context_breakdown: tokio::sync::RwLock<Option<crate::agent::context_builder::ContextBreakdown>>,
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
            context_breakdown: tokio::sync::RwLock::new(None),
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

    /// Register an in-flight request, storing the caller's REAL cancellation
    /// token so `cancel_all_requests` (graceful shutdown / SIGHUP drain) can
    /// propagate into the live `execute()` loop. Returns a [`RequestGuard`]
    /// that unregisters on drop — fixing the production leak + always-full
    /// drain timeout described on `RequestGuard`.
    ///
    /// Pass the same token the pipeline runs under (the SSE/channel/dispatcher
    /// `cancel`). For paths with no external token, pass a fresh one.
    pub fn register_request_guarded(self: &Arc<Self>, token: CancellationToken) -> RequestGuard {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.active_requests.lock().unwrap().push((id, token));
        RequestGuard {
            state: self.clone(),
            id: RequestId(id),
        }
    }

    /// Remove a previously registered request by its id. Used by
    /// [`RequestGuard::drop`].
    pub fn remove_request(&self, id: &RequestId) {
        self.active_requests
            .lock()
            .unwrap()
            .retain(|(i, _)| *i != id.0);
    }

    /// T17 triage: refresh the cached estimate-only context breakdown.
    /// Called once per `build_context` in bootstrap; best-effort, never fails.
    pub async fn set_context_breakdown(&self, breakdown: crate::agent::context_builder::ContextBreakdown) {
        *self.context_breakdown.write().await = Some(breakdown);
    }

    /// T17 triage: read the last cached context breakdown, if any turn has
    /// run since process start.
    #[allow(dead_code)] // diagnostic; sole caller was the removed GET /api/agents/{name}/context-breakdown.
    pub async fn context_breakdown(&self) -> Option<crate::agent::context_builder::ContextBreakdown> {
        self.context_breakdown.read().await.clone()
    }
}

#[cfg(test)]
impl AgentState {
    /// Old token-creating API — superseded in production by
    /// [`Self::register_request_guarded`] (which stores the real pipeline
    /// token and returns an RAII guard). Retained for the request-tracking
    /// unit tests below.
    pub fn register_request(&self) -> (RequestId, CancellationToken) {
        let token = CancellationToken::new();
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.active_requests.lock().unwrap().push((id, token.clone()));
        (RequestId(id), token)
    }

    /// Number of currently tracked active requests.
    #[cfg(test)]
    pub fn active_request_count(&self) -> usize {
        self.active_requests.lock().unwrap().len()
    }

    /// Test alias for [`Self::remove_request`].
    #[cfg(test)]
    pub fn unregister_request(&self, id: &RequestId) {
        self.remove_request(id);
    }

    /// Test-only constructor — all optional fields `None`/default/empty.
    pub fn test_new() -> Arc<Self> {
        Arc::new(Self {
            thinking_level: AtomicU8::new(0),
            channel_formatting_prompt: tokio::sync::RwLock::new(None),
            channel_info_cache: tokio::sync::RwLock::new(None),
            context_breakdown: tokio::sync::RwLock::new(None),
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
    async fn test_register_request_guarded_stores_real_token_and_drains_on_drop() {
        // R-DRAIN: the guard registers the CALLER's token (so cancel_all_requests
        // propagates into the live pipeline) and unregisters on drop (so the
        // registry doesn't leak and wait_drain returns once requests finish).
        let state = AgentState::test_new();
        let token = CancellationToken::new();
        let guard = state.register_request_guarded(token.clone());
        assert_eq!(state.active_request_count(), 1);

        // cancel_all_requests must cancel the SAME token the caller holds.
        state.cancel_all_requests();
        assert!(token.is_cancelled(), "drain must cancel the real pipeline token");

        // Dropping the guard unregisters — no leak.
        drop(guard);
        assert_eq!(state.active_request_count(), 0, "guard drop must unregister");
    }

    #[tokio::test]
    async fn test_wait_drain_returns_after_guard_drop() {
        // The guard drop is what lets a real graceful shutdown's wait_drain
        // return promptly once the in-flight turn finishes.
        let state = AgentState::test_new();
        let guard = state.register_request_guarded(CancellationToken::new());

        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(guard);
        });

        state.wait_drain(Duration::from_secs(5)).await;
        assert_eq!(state.active_request_count(), 0);
        handle.await.unwrap();
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
