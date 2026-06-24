#![deny(clippy::await_holding_lock)]
//! Mid-run clarify manager: создаёт oneshot-waiter, блокирующе ждёт ответа пользователя
//! с heartbeat и поддерживает reverse-index для сброса по сессии.
//!
//! Паттерн зеркалит `approval_manager.rs`: DashMap-шарды (sync RAII guard),
//! `#![deny(clippy::await_holding_lock)]` гарантирует отсутствие `.await`
//! при удерживании guard.
//!
//! Процесс-глобальный индекс `CLARIFY_AGENT_INDEX` (clarify_id → agent_name)
//! позволяет gateway-handler-у немедленно найти нужный движок по clarify_id,
//! не перебирая все работающие агенты (устраняет blind-scan IDOR-smell).

use std::sync::Arc;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

// ── Process-wide index: clarify_id → agent_name ─────────────────────────────

/// Глобальный индекс: clarify_id → имя агента.
/// Регистрируется при `register()`, удаляется при `resolve()` / `clear_session()` / `forget()`.
static CLARIFY_AGENT_INDEX: LazyLock<DashMap<Uuid, String>> =
    LazyLock::new(DashMap::new);

/// Вернуть имя агента, владеющего данным clarify_id.
/// `None` — waiter не существует или уже разрешён.
pub fn agent_for_clarify(id: &Uuid) -> Option<String> {
    CLARIFY_AGENT_INDEX.get(id).map(|e| e.clone())
}

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
    #[allow(dead_code)]
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
    agent_name: String,
}

impl ClarifyManager {
    pub fn new(db: PgPool, waiters: ClarifyWaitersMap, agent_name: String) -> Self {
        Self {
            db: Some(db),
            waiters,
            by_session: Arc::new(DashMap::new()),
            agent_name,
        }
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self {
            db: None,
            waiters: Arc::new(DashMap::new()),
            by_session: Arc::new(DashMap::new()),
            agent_name: "test".into(),
        }
    }

    #[allow(dead_code)]
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
        CLARIFY_AGENT_INDEX.insert(id, self.agent_name.clone());
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
            CLARIFY_AGENT_INDEX.remove(&id);
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
                CLARIFY_AGENT_INDEX.remove(&id);
                n += 1; // drop sender → receiver gets Err(RecvError) → Cancelled
            }
        }
        self.by_session.remove(&session_id);
        n
    }

    /// Cheap pre-check: есть ли вообще хоть один pending clarify-waiter.
    /// Позволяет пропустить дорогой session-lookup на каждое channel-сообщение.
    pub fn has_any_pending(&self) -> bool {
        !self.waiters.is_empty()
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

    /// Flip an existing waiter to `awaiting_text = true` so that the next
    /// plain text message from the user is intercepted as an open-ended answer.
    ///
    /// Used by the «Other» button callback: the user clicked «Other» on a
    /// choices-based clarify — we upgrade the waiter to accept free-form input.
    /// No-ops if `clarify_id` is not in the by_session index.
    pub fn mark_awaiting_text(&self, clarify_id: Uuid) {
        for mut entry in self.by_session.iter_mut() {
            for (id, at) in entry.value_mut().iter_mut() {
                if *id == clarify_id {
                    *at = true;
                    return;
                }
            }
        }
    }

    /// Remove resolved/cancelled entries from the reverse-index for `session_id`.
    fn forget(&self, session_id: Uuid) {
        // SAFETY: by_session и waiters — РАЗНЫЕ DashMap; одновременный borrow не создаёт шард-дедлок (и .await тут нет).
        if let Some(mut v) = self.by_session.get_mut(&session_id) {
            v.retain(|(id, _)| {
                if !self.waiters.contains_key(id) {
                    CLARIFY_AGENT_INDEX.remove(id);
                    false
                } else {
                    true
                }
            });
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

    #[test]
    fn has_any_pending_empty_and_after_register() {
        let m = mgr();
        assert!(!m.has_any_pending(), "empty manager must return false");
        let sid = Uuid::new_v4();
        let (_id, _rx) = m.register(sid, true);
        assert!(m.has_any_pending(), "after register must return true");
    }

    #[test]
    fn mark_awaiting_text_upgrades_choice_waiter() {
        let m = mgr();
        let sid = Uuid::new_v4();
        // Register a choices-based waiter (awaiting_text = false).
        let (btn_id, _rx) = m.register(sid, false);
        // Initially not in awaiting_text mode.
        assert_eq!(m.has_pending_text(sid), None);
        // Upgrade it to awaiting_text via «Other» button.
        m.mark_awaiting_text(btn_id);
        // Now has_pending_text should find it.
        assert_eq!(m.has_pending_text(sid), Some(btn_id));
    }

    #[test]
    fn agent_index_register_and_resolve() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (id, _rx) = m.register(sid, true);
        // After register: index contains the test agent name.
        assert_eq!(agent_for_clarify(&id).as_deref(), Some("test"));
        // After resolve: index entry is removed.
        assert!(m.resolve(id, "answer".into()));
        assert_eq!(agent_for_clarify(&id), None);
    }

    #[test]
    fn agent_index_clear_session_removes_entries() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (id, _rx) = m.register(sid, false);
        assert!(agent_for_clarify(&id).is_some());
        m.clear_session(sid);
        assert_eq!(agent_for_clarify(&id), None);
    }
}
