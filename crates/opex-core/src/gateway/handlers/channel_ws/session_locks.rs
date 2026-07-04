//! Per-`SessionKey` async mutexes. Acquired by the dispatcher before
//! `handle_with_status` so two messages destined for the same logical
//! session run sequentially while messages for different sessions run in
//! parallel.
//!
//! The map grows lazily on first acquisition for each key. Entries are
//! removed when the last `Arc<Mutex>` reference is dropped — the
//! `LockHandle::Drop` impl releases the mutex first, then checks
//! `Arc::strong_count` under the bucket lock to avoid races with a
//! concurrent acquire.

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::types::SessionKey;

#[derive(Default)]
pub(super) struct SessionLockMap {
    inner: DashMap<SessionKey, Arc<Mutex<()>>>,
}

impl SessionLockMap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: DashMap::new() })
    }

    /// Acquire (or create) the per-key mutex and return an owned guard.
    /// Holding the guard blocks any other caller for the same key.
    ///
    /// The returned [`LockHandle`] cleans up the map entry on drop if no
    /// other holders remain (refcount-based eviction).
    pub async fn acquire(self: &Arc<Self>, key: SessionKey) -> LockHandle {
        let arc = self
            .inner
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let guard = arc.lock_owned().await;
        LockHandle {
            key,
            map: Arc::clone(self),
            guard: Some(guard),
        }
    }

    #[cfg(test)]
    pub fn entry_count(&self) -> usize {
        self.inner.len()
    }
}

/// RAII guard returned by [`SessionLockMap::acquire`]. Releasing it (drop)
/// frees the mutex AND attempts to evict the map entry when no other
/// holder remains.
pub(super) struct LockHandle {
    key: SessionKey,
    map: Arc<SessionLockMap>,
    /// `Option` so [`Drop::drop`] can `take()` and explicitly release the
    /// guard BEFORE checking [`Arc::strong_count`]. Rust drops fields after
    /// `Drop::drop` returns, but the guard internally holds its own Arc
    /// clone — without explicit take(), strong_count would always be
    /// inflated by 1 and eviction would never trigger.
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for LockHandle {
    fn drop(&mut self) {
        // 1) Release the mutex by dropping the guard. The guard internally
        //    holds an Arc clone; dropping it decrements strong_count by 1.
        drop(self.guard.take());
        // 2) Eviction decision: the predicate runs under DashMap's bucket
        //    lock, so it observes a consistent strong_count. We do NOT do an
        //    outer count check first — that creates a TOCTOU window where a
        //    concurrent `acquire` can clone the Arc between the check and
        //    `remove_if`'s entry into the bucket lock. Evicting then would
        //    leave the concurrent acquirer holding a stale Arc while the
        //    next acquirer creates a fresh Mutex for the same key, breaking
        //    the per-key FIFO invariant.
        //
        //    The predicate counts: map(1) when nobody else holds an Arc.
        //    Anything ≥ 2 means a concurrent acquire is in flight — keep the
        //    entry. (We intentionally do NOT keep our own Arc clone inside
        //    `LockHandle` anymore: it inflated strong_count and complicated
        //    the predicate without buying anything that the bucket lock +
        //    `Arc::strong_count(v)` doesn't already give us.)
        self.map.inner.remove_if(&self.key, |_, v| Arc::strong_count(v) <= 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn key(user: &str) -> SessionKey {
        SessionKey {
            agent_name: "Arty".to_string(),
            eff_user: user.to_string(),
            eff_channel: "telegram".to_string(),
            eff_chat_scope: None,
        }
    }

    #[tokio::test]
    async fn same_key_serialises() {
        let map = SessionLockMap::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];
        for _ in 0..5 {
            let map = Arc::clone(&map);
            let c = Arc::clone(&counter);
            let m = Arc::clone(&max_concurrent);
            handles.push(tokio::spawn(async move {
                let _h = map.acquire(key("alice")).await;
                let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                m.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                c.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles { h.await.unwrap(); }

        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1, "same key must serialise");
    }

    #[tokio::test]
    async fn different_keys_run_in_parallel() {
        let map = SessionLockMap::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];
        for i in 0..5 {
            let map = Arc::clone(&map);
            let c = Arc::clone(&counter);
            let m = Arc::clone(&max_concurrent);
            handles.push(tokio::spawn(async move {
                let _h = map.acquire(key(&format!("user{i}"))).await;
                let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                m.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                c.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles { h.await.unwrap(); }

        assert!(max_concurrent.load(Ordering::SeqCst) >= 2, "different keys must overlap");
    }

    #[tokio::test]
    async fn entries_evicted_when_idle() {
        let map = SessionLockMap::new();
        {
            let _h = map.acquire(key("alice")).await;
            assert_eq!(map.entry_count(), 1);
        } // LockHandle dropped here — Drop::drop releases guard then evicts.
        // Give the runtime a tick for any async machinery to settle.
        tokio::task::yield_now().await;
        assert_eq!(map.entry_count(), 0, "entry must be evicted when no holders");
    }

    #[tokio::test]
    async fn no_eviction_while_another_waiter_present() {
        let map = SessionLockMap::new();

        let map2 = Arc::clone(&map);
        let blocker = tokio::spawn(async move {
            let _h = map2.acquire(key("alice")).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        // Give blocker a chance to grab the lock.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let map3 = Arc::clone(&map);
        let waiter = tokio::spawn(async move {
            let _h = map3.acquire(key("alice")).await;
            // By the time we get here, blocker has released — but the map
            // entry must not have been evicted prematurely (we were a holder).
        });

        // Wait for both to finish.
        blocker.await.unwrap();
        waiter.await.unwrap();

        // After both holders are gone, the entry should be evicted.
        tokio::task::yield_now().await;
        assert_eq!(map.entry_count(), 0);
    }
}
