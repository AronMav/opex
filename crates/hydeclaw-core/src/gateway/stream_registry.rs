use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sqlx::PgPool;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::stream_jobs;

const BROADCAST_CAPACITY: usize = 1024;
const MAX_BUFFER_SIZE: usize = 1_000;
/// Max concurrent active streams (prevent memory exhaustion on Pi)
const MAX_ACTIVE_STREAMS: usize = 50;

/// Mutable per-stream state, protected by its own Mutex.
struct ActiveStreamInner {
    events: Vec<(u64, String)>,
    finished_at: Option<Instant>,
    next_event_id: u64,
}

/// A single SSE stream. `broadcast_tx` is outside the Mutex because
/// `broadcast::Sender::send` only requires `&self`.
struct ActiveStream {
    inner: Mutex<ActiveStreamInner>,
    broadcast_tx: broadcast::Sender<(u64, String)>,
    cancel_token: CancellationToken,
    /// Lock-free finished flag — prevents deadlock in eviction
    finished: AtomicBool,
    #[allow(dead_code)]
    session_id: Uuid,
    /// Link to `stream_jobs` row in `PostgreSQL` for persistence.
    pub job_id: Uuid,
    #[allow(dead_code)]
    created_at: Instant,
}

// AUDIT:SSE-03 (verified 2026-03-30): Session cleanup on disconnect:
// 1. register() cancels any prior stream for the same session (no duplicates) -- see line ~79
// 2. Client disconnect does NOT abort engine (intentional: saves response to DB)
//    The send_and_buffer! macro in chat.rs detects sse_tx.is_closed() and sets
//    client_gone_since, but continues buffering events for DB persistence.
// 3. 10-minute timeout (client_gone_since > 600s) aborts engine if client never reconnects
//    -- see chat.rs converter loop safety net check
// 4. Cancel API (POST /api/chat/{id}/abort) sets CancellationToken, checked each iteration
//    in the converter loop -- provides immediate user-initiated cancellation
// No hanging tasks possible: either client reconnects, timeout fires, or cancel API used.
pub struct StreamRegistry {
    streams: RwLock<HashMap<String, Arc<ActiveStream>>>,
    db: PgPool,
}

impl StreamRegistry {
    pub fn new(db: PgPool) -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            db,
        }
    }

    /// Like the old `register`, but uses a caller-supplied `CancellationToken` instead of
    /// creating a new one. The caller must hold another clone of the token to detect
    /// cancellation (e.g. pass one clone to the pipeline, register the other here).
    /// Returns `job_id` on success, `None` on capacity limit or DB error.
    pub async fn register_with_token(
        &self,
        session_id: Uuid,
        agent_id: &str,
        cancel_token: CancellationToken,
    ) -> Option<Uuid> {
        let job_id = match stream_jobs::create_job(&self.db, session_id, agent_id).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(error = %e, "failed to create stream job in DB");
                return None;
            }
        };

        let key = session_id.to_string();
        let (broadcast_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let mut streams = self.streams.write().await;

        if streams.len() >= MAX_ACTIVE_STREAMS {
            streams.retain(|_, s| !s.finished.load(Ordering::Relaxed));
            if streams.len() >= MAX_ACTIVE_STREAMS {
                tracing::warn!("stream registry at capacity ({MAX_ACTIVE_STREAMS}), rejecting new stream");
                return None;
            }
        }

        if let Some(existing) = streams.get(&key) {
            existing.cancel_token.cancel();
            existing.finished.store(true, Ordering::Relaxed);
            let mut inner = existing.inner.lock().await;
            inner.finished_at = Some(Instant::now());
            if let Err(e) =
                stream_jobs::error_job(&self.db, existing.job_id, "superseded by new stream").await
            {
                tracing::warn!(error = %e, "stream_registry: failed to mark superseded job as error");
            }
        }

        streams.insert(
            key,
            Arc::new(ActiveStream {
                inner: Mutex::new(ActiveStreamInner {
                    events: Vec::new(),
                    finished_at: None,
                    next_event_id: 0,
                }),
                broadcast_tx,
                cancel_token,
                finished: AtomicBool::new(false),
                session_id,
                job_id,
                created_at: Instant::now(),
            }),
        );

        Some(job_id)
    }

    /// Cancel an active stream. Returns true if found and cancelled.
    pub async fn cancel(&self, session_id: &str) -> bool {
        let streams = self.streams.read().await;
        if let Some(stream) = streams.get(session_id) {
            stream.cancel_token.cancel();
            true
        } else {
            false
        }
    }

    /// Push an SSE JSON event string into the buffer and broadcast to subscribers.
    /// Returns the assigned monotonic event ID.
    ///
    /// Uses a **read lock** on the `HashMap` (concurrent with other streams)
    /// plus a per-stream Mutex (serializes events within the same stream).
    pub async fn push_event(&self, session_id: &str, event_json: &str) -> u64 {
        let streams = self.streams.read().await;
        if let Some(stream) = streams.get(session_id) {
            let mut inner = stream.inner.lock().await;
            let id = inner.next_event_id;
            inner.next_event_id += 1;
            let owned = event_json.to_owned();
            if inner.events.len() < MAX_BUFFER_SIZE {
                // Buffer + broadcast
                inner.events.push((id, owned.clone()));
                let _ = stream.broadcast_tx.send((id, owned));
            } else {
                // Buffer full: broadcast only
                let _ = stream.broadcast_tx.send((id, owned));
            }
            id
        } else {
            0
        }
    }

    /// Set in-memory finished state and return `job_id` (under lock).
    async fn set_finished_state(&self, session_id: &str) -> Option<Uuid> {
        let streams = self.streams.read().await;
        let stream = streams.get(session_id)?;
        stream.finished.store(true, Ordering::Relaxed);
        let mut inner = stream.inner.lock().await;
        inner.finished_at = Some(Instant::now());
        Some(stream.job_id)
    }

    /// Mark a stream as finished. DB write happens after releasing locks.
    pub async fn mark_finished(&self, session_id: &str) {
        if let Some(jid) = self.set_finished_state(session_id).await
            && let Err(e) = stream_jobs::finish_job(&self.db, jid).await {
                tracing::warn!(session = %session_id, error = %e, "stream_registry: failed to mark job as finished");
            }
    }

    /// Mark a stream as error. DB write happens after releasing locks.
    pub async fn mark_error(&self, session_id: &str, error: &str) {
        if let Some(jid) = self.set_finished_state(session_id).await
            && let Err(e) = stream_jobs::error_job(&self.db, jid, error).await {
                tracing::warn!(session = %session_id, error = %e, "stream_registry: failed to mark job as error");
            }
    }

    /// Get the DB pool for direct job queries (used by resume handler).
    pub fn db(&self) -> &PgPool {
        &self.db
    }

    /// Phase 65 OBS-05: snapshot the number of registered streams for
    /// `/api/health/dashboard`. Briefly acquires the read lock on the
    /// streams map — safe to call from a request handler.
    pub async fn snapshot_size(&self) -> u64 {
        self.streams.read().await.len() as u64
    }

    /// Subscribe to a stream: returns (buffered events snapshot, broadcast receiver, `is_finished`).
    ///
    /// The subscribe + snapshot is atomic (same per-stream lock) to prevent event loss.
    /// The receiver is created BEFORE the snapshot, so any events pushed between
    /// `subscribe()` and the first `recv()` will appear in both the snapshot and the receiver.
    /// The caller must deduplicate using the snapshot length as an offset.
    pub async fn subscribe(
        &self,
        session_id: &str,
    ) -> Option<(Vec<(u64, String)>, broadcast::Receiver<(u64, String)>, bool)> {
        let streams = self.streams.read().await;
        let stream = streams.get(session_id)?;
        let inner = stream.inner.lock().await;
        // Subscribe first (under per-stream lock), then snapshot — guarantees no gap
        let rx = stream.broadcast_tx.subscribe();
        let snapshot = inner.events.clone();
        let finished = stream.finished.load(Ordering::Relaxed);
        Some((snapshot, rx, finished))
    }

    /// Remove finished streams older than `max_age`.
    /// Two-phase: read lock to identify candidates, write lock only for removal.
    pub async fn cleanup(&self, max_age: Duration) {
        let now = Instant::now();
        // Phase 1: read lock — identify expired streams (only check those marked finished)
        let keys_to_remove = {
            let streams = self.streams.read().await;
            let mut keys = Vec::new();
            for (key, stream) in streams.iter() {
                if !stream.finished.load(Ordering::Relaxed) {
                    continue;
                }
                let inner = stream.inner.lock().await;
                if let Some(finished_at) = inner.finished_at
                    && now.duration_since(finished_at) >= max_age {
                        keys.push(key.clone());
                    }
            }
            keys
        };
        // Phase 2: write lock only if there's something to remove
        if !keys_to_remove.is_empty() {
            let mut streams = self.streams.write().await;
            for key in &keys_to_remove {
                streams.remove(key);
            }
        }
    }
}

// Tests require a running PostgreSQL instance (register() creates DB jobs).
// Run with: cargo test -- --ignored stream_registry
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_nonexistent_returns_false() {
        // This test doesn't need DB — cancel only reads in-memory map
        let db = PgPool::connect_lazy("postgres://invalid").unwrap();
        let registry = StreamRegistry::new(db);
        assert!(!registry.cancel("nonexistent").await);
    }
}
