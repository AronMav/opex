use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sqlx::PgPool;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::stream_jobs;

// Single-user system, buffer must hold a full tool-turn worth of events — see
// spec §5.3. Events are held TWICE — once in the `events` Vec and once in the
// broadcast ring (the ring retains up to capacity even with zero receivers) —
// i.e. ~2x the bytes of buffered events × up to MAX_ACTIVE_STREAMS concurrent
// streams. This is a deliberate tradeoff, not an oversight.
const BROADCAST_CAPACITY: usize = 10_000;
const MAX_BUFFER_SIZE: usize = 10_000;
/// Hysteresis threshold for overflow compaction: a compaction pass must shrink
/// the buffer DECISIVELY (below half capacity) for buffering to continue.
/// Without it, a marginally-mergeable flood (interleaved block ids where only
/// a few adjacent pairs merge per pass) would shrink slightly, refill in a few
/// pushes, and re-trigger a full O(n) JSON-parse compaction under the
/// per-stream lock indefinitely. With the target, such floods hit the sticky
/// `truncated` fallback after ONE pass. A genuine single/few-block delta flood
/// compacts 10k → a handful of events — far below the target — so the normal
/// case is unaffected.
const COMPACTION_TARGET: usize = MAX_BUFFER_SIZE / 2;
/// Max concurrent active streams (prevent memory exhaustion on Pi)
const MAX_ACTIVE_STREAMS: usize = 50;

/// Mutable per-stream state, protected by its own Mutex.
struct ActiveStreamInner {
    events: Vec<(u64, String)>,
    finished_at: Option<Instant>,
    next_event_id: u64,
    /// Set when the buffer overflowed AND compaction could not shrink the
    /// buffer decisively (below half capacity — `COMPACTION_TARGET`), e.g. a
    /// non-delta or interleaved-id flood. Late subscribers get an incomplete
    /// replay; the client shows a banner and relies on the final history
    /// refetch.
    truncated: bool,
}

/// A single SSE stream. `broadcast_tx` is outside the Mutex because
/// `broadcast::Sender::send` only requires `&self`.
#[allow(dead_code)] // session_id/created_at are diagnostic metadata, not read at runtime.
struct ActiveStream {
    inner: Mutex<ActiveStreamInner>,
    broadcast_tx: broadcast::Sender<(u64, String)>,
    cancel_token: CancellationToken,
    /// Lock-free finished flag — prevents deadlock in eviction
    finished: AtomicBool,
    session_id: Uuid,
    /// Link to `stream_jobs` row in `PostgreSQL` for persistence.
    pub job_id: Uuid,
    created_at: Instant,
    /// Boundary message id (the user message that opened this turn) — carried
    /// through to `subscribe()` so late subscribers (T4) can build the sync
    /// envelope without a separate DB round-trip.
    boundary_message_id: Uuid,
}

// AUDIT:SSE-03 (verified 2026-03-30): Session cleanup on disconnect:
// 1. register() cancels any prior stream for the same session (no duplicates) -- see line ~79
// 2. Client disconnect does NOT abort engine (intentional: saves response to DB).
//    The send_and_buffer! macro in sse_converter.rs detects sse_tx.is_closed()
//    and keeps buffering events for DB persistence while the engine runs to
//    natural completion. There is NO client-gone timeout-abort — a browser drop
//    is a transport event, not a cancel.
// 3. Cancel API (POST /api/chat/{id}/abort) sets CancellationToken, checked each
//    iteration in the converter loop -- provides immediate user-initiated cancel
//    (with a 30s grace window, then hard-abort if wedged).
// Runaway protection is the engine's own: max_iterations, loop-detection, tool
// timeouts. A run stops via natural completion, those limits, or explicit cancel.
pub struct StreamRegistry {
    streams: RwLock<HashMap<String, Arc<ActiveStream>>>,
    db: PgPool,
}

/// Result of `StreamRegistry::subscribe` — buffered events + live receiver + metadata
/// needed by callers (T4's resume/GET-stream handler) to build the sync envelope
/// without a separate DB round-trip.
pub struct StreamSubscription {
    pub events: Vec<(u64, String)>,
    pub rx: broadcast::Receiver<(u64, String)>,
    pub finished: bool,
    pub boundary_message_id: Uuid,
    pub truncated: bool,
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
        boundary_message_id: Uuid,
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
            // Task 2 (Session Resilience): pre-mark the superseded session
            // `interrupted` with an explicit reason immediately, rather than
            // relying solely on the cooperative cancel_token reaching
            // `finalize` (reason "cancel_token") or, failing that, the
            // `SessionLifecycleGuard::Drop` backstop (reason "guard dropped
            // (early exit)"). Mirrors the SSE cancel-grace pre-mark in
            // `sse_converter.rs`. Safe to fire immediately even though the old
            // stream's engine task is only cooperatively cancelled here (not
            // aborted) and may still be mid-flight: `cleanup_session_terminated`
            // performs an atomic running→interrupted claim, and the guard's own
            // `done()`/`fail()`/`interrupt()` transitions are no-ops at the DB
            // level once run_status is no longer 'running' (their
            // `set_session_run_status` call is WHERE-guarded on
            // run_status='running') — so a late-finishing superseded turn can
            // never clobber this explicit 'superseded' claim.
            if let Err(e) = crate::db::sessions::cleanup_session_terminated(
                &self.db,
                existing.session_id,
                "interrupted",
                "superseded",
            )
            .await
            {
                tracing::warn!(
                    session_id = %existing.session_id,
                    error = %e,
                    "stream_registry: failed to pre-mark superseded session interrupted"
                );
            }
        }

        streams.insert(
            key,
            Arc::new(ActiveStream {
                inner: Mutex::new(ActiveStreamInner {
                    events: Vec::new(),
                    finished_at: None,
                    next_event_id: 0,
                    truncated: false,
                }),
                broadcast_tx,
                cancel_token,
                finished: AtomicBool::new(false),
                session_id,
                job_id,
                created_at: Instant::now(),
                boundary_message_id,
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
            // Buffer full: compact adjacent text-deltas in place (replay stays
            // semantically complete). Hysteresis: buffering continues only if
            // the pass shrank the buffer decisively (below COMPACTION_TARGET);
            // a marginal shrink would refill in a few pushes and re-trigger a
            // full O(n) compaction under the lock indefinitely. Otherwise
            // `truncated` becomes the sticky fallback — once set we stop
            // re-attempting compaction on every push.
            if inner.events.len() >= MAX_BUFFER_SIZE && !inner.truncated {
                compact_events(&mut inner.events);
                if inner.events.len() >= COMPACTION_TARGET {
                    inner.truncated = true;
                }
            }
            if !inner.truncated && inner.events.len() < MAX_BUFFER_SIZE {
                inner.events.push((id, owned.clone()));
                let _ = stream.broadcast_tx.send((id, owned));
            } else {
                // Pathological (uncompactable) overflow: broadcast only.
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

    /// Subscribe to a stream: buffered events snapshot + broadcast receiver + metadata.
    ///
    /// The subscribe + snapshot is atomic (same per-stream lock) to prevent event loss.
    /// The receiver is created BEFORE the snapshot, so any events pushed between
    /// `subscribe()` and the first `recv()` will appear in both the snapshot and the receiver.
    /// The caller must deduplicate using the snapshot length as an offset.
    pub async fn subscribe(&self, session_id: &str) -> Option<StreamSubscription> {
        let streams = self.streams.read().await;
        let stream = streams.get(session_id)?;
        let inner = stream.inner.lock().await;
        // Subscribe first (under per-stream lock), then snapshot — guarantees no gap.
        // boundary_message_id/truncated are read under the same per-stream lock as
        // the events snapshot, so all four fields are atomic with each other.
        let rx = stream.broadcast_tx.subscribe();
        let events = inner.events.clone();
        let finished = stream.finished.load(Ordering::Relaxed);
        Some(StreamSubscription {
            events,
            rx,
            finished,
            boundary_message_id: stream.boundary_message_id,
            truncated: inner.truncated,
        })
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

/// Compact the replay buffer in place: adjacent `text-delta` events of the
/// same block `id` merge into one event with the concatenated delta. The
/// merged event keeps the seq of its LAST constituent — seq stays monotonic,
/// so subscriber seq-cutoff (stream.rs) keeps working unchanged.
fn compact_events(events: &mut Vec<(u64, String)>) {
    let mut out: Vec<(u64, String)> = Vec::with_capacity(events.len() / 4);
    for (seq, json) in events.drain(..) {
        if let Some(last) = out.last_mut()
            && let Some(merged) = try_merge_text_delta(&last.1, &json)
        {
            last.0 = seq;
            last.1 = merged;
            continue;
        }
        out.push((seq, json));
    }
    *events = out;
}

/// Merge two adjacent SSE JSON strings when both are `text-delta` of the
/// same block id. Returns the merged JSON, or None when not mergeable.
/// Both events must carry a string `id` — two id-less deltas never merge.
fn try_merge_text_delta(prev: &str, next: &str) -> Option<String> {
    let p: serde_json::Value = serde_json::from_str(prev).ok()?;
    if p.get("type")?.as_str()? != "text-delta" {
        return None;
    }
    let n: serde_json::Value = serde_json::from_str(next).ok()?;
    if n.get("type")?.as_str()? != "text-delta" {
        return None;
    }
    if p.get("id")?.as_str()? != n.get("id")?.as_str()? {
        return None;
    }
    let combined = format!("{}{}", p.get("delta")?.as_str()?, n.get("delta")?.as_str()?);
    let mut merged = p;
    merged["delta"] = serde_json::Value::String(combined);
    serde_json::to_string(&merged).ok()
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

    /// Task 2 (Session Resilience, G1): registering a new stream for a
    /// session that already has an active stream (supersede) must
    /// immediately pre-mark the OLD session `interrupted` with the explicit
    /// reason `superseded` — not left to the cooperative cancel_token or the
    /// `SessionLifecycleGuard::Drop` backstop to eventually resolve it.
    #[sqlx::test(migrations = "../../migrations")]
    async fn supersede_pre_marks_old_session_interrupted(pool: sqlx::PgPool) {
        let registry = StreamRegistry::new(pool.clone());
        let sid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, run_status) \
             VALUES ($1, 'A', 'u', 'ui', 'running')",
        )
        .bind(sid)
        .execute(registry.db())
        .await
        .expect("seed session");

        let old_token = CancellationToken::new();
        registry
            .register_with_token(sid, "A", old_token.clone(), Uuid::new_v4())
            .await
            .expect("first register");

        // A second registration for the SAME session_id supersedes the first.
        let new_token = CancellationToken::new();
        registry
            .register_with_token(sid, "A", new_token, Uuid::new_v4())
            .await
            .expect("second register (supersede)");

        assert!(
            old_token.is_cancelled(),
            "supersede must cancel the old stream's token cooperatively"
        );

        let run_status: Option<String> =
            sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
                .bind(sid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            run_status.as_deref(),
            Some("interrupted"),
            "superseded session must be pre-marked interrupted immediately, not left running"
        );

        let payload: serde_json::Value = sqlx::query_scalar(
            "SELECT payload FROM session_timeline \
             WHERE session_id = $1 AND event_type = 'interrupted' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(sid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            payload["reason"], "superseded",
            "explicit reason must be 'superseded', not the generic cancel_token/guard_dropped reasons"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn subscribe_carries_boundary_and_truncated(pool: sqlx::PgPool) {
        let registry = StreamRegistry::new(pool);
        let sid = Uuid::new_v4();
        // stream_jobs.session_id has an FK to sessions(id); seed a row first.
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'A', 'u', 'ui')")
            .bind(sid)
            .execute(registry.db())
            .await
            .expect("seed session");
        let boundary = Uuid::new_v4();
        let token = CancellationToken::new();
        registry
            .register_with_token(sid, "A", token, boundary)
            .await
            .expect("register");
        let sub = registry.subscribe(&sid.to_string()).await.expect("subscribed");
        assert_eq!(sub.boundary_message_id, boundary);
        assert!(!sub.truncated);
        assert!(sub.events.is_empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn overflow_sets_truncated(pool: sqlx::PgPool) {
        let registry = StreamRegistry::new(pool);
        let sid = Uuid::new_v4();
        // stream_jobs.session_id has an FK to sessions(id); seed a row first.
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'A', 'u', 'ui')")
            .bind(sid)
            .execute(registry.db())
            .await
            .expect("seed session");
        registry
            .register_with_token(sid, "A", CancellationToken::new(), Uuid::new_v4())
            .await
            .unwrap();
        let key = sid.to_string();
        for i in 0..(10_000 + 5) {
            registry.push_event(&key, &format!("{{\"i\":{i}}}")).await;
        }
        let sub = registry.subscribe(&key).await.unwrap();
        assert_eq!(sub.events.len(), 10_000);
        assert!(sub.truncated);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn overflow_compacts_text_deltas(pool: sqlx::PgPool) {
        let registry = StreamRegistry::new(pool);
        let sid = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'A', 'u', 'ui')")
            .bind(sid)
            .execute(registry.db())
            .await
            .expect("seed session");
        registry
            .register_with_token(sid, "A", CancellationToken::new(), Uuid::new_v4())
            .await
            .unwrap();
        let key = sid.to_string();
        let total: u64 = 10_000 + 500;
        for i in 0..total {
            registry
                .push_event(&key, &format!("{{\"type\":\"text-delta\",\"id\":\"t1\",\"delta\":\"x{i};\"}}"))
                .await;
        }
        let sub = registry.subscribe(&key).await.unwrap();
        // Компакция должна была ужать буфер — replay полный, без truncated.
        assert!(!sub.truncated, "delta stream must compact, not truncate");
        assert!(sub.events.len() < MAX_BUFFER_SIZE);
        // seq строго монотонен и последний seq сохранён.
        assert!(sub.events.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(sub.events.last().unwrap().0, total - 1);
        // Семантическая полнота: конкатенация всех delta == исходный текст.
        let mut concat = String::new();
        for (_, json) in &sub.events {
            let v: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(v["type"], "text-delta");
            concat.push_str(v["delta"].as_str().unwrap());
        }
        let expected: String = (0..total).map(|i| format!("x{i};")).collect();
        assert_eq!(concat, expected);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn overflow_with_alternating_ids_truncates(pool: sqlx::PgPool) {
        let registry = StreamRegistry::new(pool);
        let sid = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'A', 'u', 'ui')")
            .bind(sid)
            .execute(registry.db())
            .await
            .expect("seed session");
        registry
            .register_with_token(sid, "A", CancellationToken::new(), Uuid::new_v4())
            .await
            .unwrap();
        let key = sid.to_string();
        // Строго чередующиеся block id — ни одна соседняя пара не сливается,
        // компакция = no-op → гистерезис переводит в sticky truncated.
        for i in 0..(10_000u64 + 10) {
            let id = if i % 2 == 0 { "a" } else { "b" };
            registry
                .push_event(&key, &format!("{{\"type\":\"text-delta\",\"id\":\"{id}\",\"delta\":\"x{i};\"}}"))
                .await;
        }
        let sub = registry.subscribe(&key).await.unwrap();
        assert!(sub.truncated, "uncompactable flood must fall back to truncated");
        // Буфер цел до потолка, дальше — только broadcast (send-only).
        assert_eq!(sub.events.len(), 10_000);
    }
}
