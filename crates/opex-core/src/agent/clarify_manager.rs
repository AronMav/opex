#![deny(clippy::await_holding_lock)]
//! Mid-run clarify manager: создаёт oneshot-waiter, блокирующе ждёт ответа пользователя
//! с heartbeat и поддерживает reverse-index для сброса по сессии.
//!
//! Паттерн зеркалит `approval_manager.rs`: DashMap-шарды (sync RAII guard),
//! `#![deny(clippy::await_holding_lock)]` гарантирует отсутствие `.await`
//! при удерживании guard.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

// ── Types ────────────────────────────────────────────────────────────────────

/// Map of pending clarify waiters: clarify_id → (oneshot sender, creation time).
pub type ClarifyWaitersMap =
    Arc<DashMap<Uuid, (tokio::sync::oneshot::Sender<String>, Instant)>>;

#[derive(Debug)]
pub enum NoResponseReason {
    TimedOut,
    Cancelled,
}

#[derive(Debug)]
pub enum ClarifyOutcome {
    Answered(String),
    NoResponse(NoResponseReason),
}

// ── ClarifyManager ───────────────────────────────────────────────────────────

/// Manager of mid-run clarification waiters.
///
/// `by_session` is a reverse-index: session_id → [(clarify_id, awaiting_text)].
/// `awaiting_text = true` means the waiter is open-ended (no choices);
/// `awaiting_text = false` means it's a choice/button prompt.
pub struct ClarifyManager {
    db: Option<PgPool>,
    waiters: ClarifyWaitersMap,
    by_session: Arc<DashMap<Uuid, Vec<(Uuid, bool)>>>,
}

impl ClarifyManager {
    pub fn new(db: PgPool, waiters: ClarifyWaitersMap) -> Self {
        Self {
            db: Some(db),
            waiters,
            by_session: Arc::new(DashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self {
            db: None,
            waiters: Arc::new(DashMap::new()),
            by_session: Arc::new(DashMap::new()),
        }
    }

    pub fn waiters(&self) -> &ClarifyWaitersMap {
        &self.waiters
    }

    /// Create a waiter; returns (clarify_id, receiver).
    ///
    /// `awaiting_text = true` means open-ended (no choices).
    /// Does not block — caller must call `wait_rx` separately so delivery
    /// (Task 5/6) can be inserted between register and wait.
    pub fn register(
        &self,
        session_id: Uuid,
        awaiting_text: bool,
    ) -> (Uuid, tokio::sync::oneshot::Receiver<String>) {
        let id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiters.insert(id, (tx, Instant::now()));
        self.by_session
            .entry(session_id)
            .or_default()
            .push((id, awaiting_text));
        (id, rx)
    }

    /// Block on `rx` until resolved or timeout, touching session activity each ~1s.
    ///
    /// Heartbeat: each `tokio::time::timeout(min(1s, remaining), &mut rx)` expiry
    /// triggers `touch_session_activity` so the session doesn't appear stale.
    /// In tests (`db = None`) the touch is skipped.
    pub async fn wait_rx(
        &self,
        mut rx: tokio::sync::oneshot::Receiver<String>,
        session_id: Uuid,
        timeout: Duration,
    ) -> ClarifyOutcome {
        let deadline = Instant::now() + timeout;
        let out = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break ClarifyOutcome::NoResponse(NoResponseReason::TimedOut);
            }
            match tokio::time::timeout(remaining.min(Duration::from_secs(1)), &mut rx).await {
                Ok(Ok(answer)) => break ClarifyOutcome::Answered(answer),
                Ok(Err(_)) => break ClarifyOutcome::NoResponse(NoResponseReason::Cancelled),
                Err(_elapsed) => {
                    // Heartbeat: touch session so it doesn't expire while waiting.
                    if let Some(db) = &self.db {
                        let _ =
                            crate::db::sessions::touch_session_activity(db, session_id).await;
                    }
                }
            }
        };
        self.forget(session_id);
        out
    }

    /// Resolve a pending waiter with the user's response.
    ///
    /// Returns `true` if the waiter was found and the answer delivered.
    pub fn resolve(&self, id: Uuid, response: String) -> bool {
        if let Some((_, (tx, _))) = self.waiters.remove(&id) {
            tx.send(response).is_ok()
        } else {
            false
        }
    }

    /// Drop all waiters for `session_id` (sender drop → `Cancelled` on the receiver).
    ///
    /// Returns the number of waiters cancelled.
    pub fn clear_session(&self, session_id: Uuid) -> usize {
        let ids: Vec<Uuid> = self
            .by_session
            .get(&session_id)
            .map(|v| v.iter().map(|(id, _)| *id).collect())
            .unwrap_or_default();
        let mut n = 0;
        for id in ids {
            if self.waiters.remove(&id).is_some() {
                n += 1; // drop sender → receiver gets Err(RecvError) → Cancelled
            }
        }
        self.by_session.remove(&session_id);
        n
    }

    /// Return the first open-ended (awaiting_text) waiter id for `session_id`,
    /// or `None` if there is none.
    pub fn has_pending_text(&self, session_id: Uuid) -> Option<Uuid> {
        self.by_session.get(&session_id).and_then(|v| {
            v.iter()
                .find(|(id, at)| *at && self.waiters.contains_key(id))
                .map(|(id, _)| *id)
        })
    }

    /// Remove resolved/cancelled entries from the reverse-index for `session_id`.
    fn forget(&self, session_id: Uuid) {
        if let Some(mut v) = self.by_session.get_mut(&session_id) {
            v.retain(|(id, _)| self.waiters.contains_key(id));
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn mgr() -> ClarifyManager {
        ClarifyManager::new_for_test()
    }

    #[tokio::test]
    async fn register_resolve_returns_answer() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (id, rx) = m.register(sid, true);
        assert!(m.resolve(id, "blue".into()));
        let out = m.wait_rx(rx, sid, Duration::from_secs(5)).await;
        assert!(matches!(out, ClarifyOutcome::Answered(a) if a == "blue"));
    }

    #[tokio::test]
    async fn timeout_yields_no_response() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (_id, rx) = m.register(sid, true);
        let out = m.wait_rx(rx, sid, Duration::from_millis(50)).await;
        assert!(matches!(
            out,
            ClarifyOutcome::NoResponse(NoResponseReason::TimedOut)
        ));
    }

    #[tokio::test]
    async fn clear_session_cancels_pending() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (_id, rx) = m.register(sid, false);
        assert_eq!(m.clear_session(sid), 1);
        let out = m.wait_rx(rx, sid, Duration::from_secs(5)).await;
        assert!(matches!(
            out,
            ClarifyOutcome::NoResponse(NoResponseReason::Cancelled)
        ));
    }

    #[test]
    fn has_pending_text_returns_open_ended_only() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (_btn, _rx1) = m.register(sid, false); // choices present → not awaiting_text
        let (open, _rx2) = m.register(sid, true); // open-ended → awaiting_text
        assert_eq!(m.has_pending_text(sid), Some(open));
    }
}
