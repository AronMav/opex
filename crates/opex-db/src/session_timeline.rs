//! Session timeline — chronological log of session lifecycle events.
//!
//! During normal operation, session state transitions (running, `tool_start`,
//! `tool_end`, done, failed) are appended to `session_timeline`. The table is
//! used for:
//!   * LoopDetector warm-up on session re-entry (preserves loop-break
//!     decisions across restarts — see `load_tool_events`).
//!   * Diagnostics: a per-session audit trail of what happened and when.
//!   * The UI Timeline view (future).
//!
//! This is NOT a Write-Ahead Log: there is no replay-based recovery. On
//! crash, completed work is preserved by the persisted side effects
//! (workspace files, memory chunks, channel messages, DB rows), not by
//! replaying events from this table. The `session_events` legacy name and
//! "WAL" framing have been retired (migration m049).

use anyhow::Result;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Log a session lifecycle event. Standalone variant — opens its own
/// transaction. Use `log_event_tx` when already inside a transaction.
pub async fn log_event(
    db: &PgPool,
    session_id: Uuid,
    event_type: &str,
    payload: Option<&serde_json::Value>,
) -> Result<()> {
    let mut tx = db.begin().await?;
    log_event_tx(&mut tx, session_id, event_type, payload).await?;
    tx.commit().await?;
    Ok(())
}

/// In-transaction variant of [`log_event`]. Appends a timeline row and
/// refreshes `activity_at` (debounced to ~10 s resolution).
///
/// Use this when you need to combine the timeline write with other DB
/// operations in a single transaction (e.g. a multi-step cleanup that
/// must be atomic). Standalone callers should use [`log_event`].
pub async fn log_event_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    event_type: &str,
    payload: Option<&serde_json::Value>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_timeline (session_id, event_type, payload) VALUES ($1, $2, $3)",
    )
    .bind(session_id)
    .bind(event_type)
    .bind(payload)
    .execute(&mut **tx)
    .await?;

    // Heartbeat — debounced to ~10s resolution.
    //
    // Without debounce a busy session with 5 parallel tools + 3 LLM calls/sec
    // would emit 15-20 timeline events/sec, each triggering an UPDATE sessions.
    // Postgres row-locks on the sessions row would serialise concurrent
    // tool-end writes and stall throughput. Watchdog polls every 60s, so
    // a 10s heartbeat granularity is more than enough.
    //
    // Guard `run_status = 'running'` so terminal sessions are not
    // accidentally resurrected.
    sqlx::query(
        "UPDATE sessions SET activity_at = NOW()
         WHERE id = $1
           AND run_status = 'running'
           AND (activity_at IS NULL OR activity_at < NOW() - INTERVAL '10 seconds')",
    )
    .bind(session_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Phase 62 RES-03: batched DELETE for `session_timeline` rows older than `days`.
///
/// PostgreSQL has no native `DELETE ... LIMIT`. We wrap with
/// `DELETE FROM t WHERE id IN (SELECT id FROM t WHERE <cond> ORDER BY id LIMIT N)`
/// and loop until `rows_affected < batch_size`.
///
/// This avoids long table locks and WAL bloat during large cleanups — each
/// iteration locks only `batch_size` rows, keeping autovacuum happy and
/// letting the tokio scheduler interleave other work between batches.
///
/// Returns the total number of rows deleted across all batches.
///
/// # Errors
/// Returns an error if `batch_size <= 0` (guard against accidental "delete all"
/// via negative/zero LIMIT that PostgreSQL would reject at execution time).
pub async fn prune_old_events_batched(
    db: &PgPool,
    days: u32,
    batch_size: i64,
) -> Result<u64> {
    if days == 0 {
        return Ok(0);
    }
    if batch_size <= 0 {
        anyhow::bail!("batch_size must be > 0, got {batch_size}");
    }
    let mut total_deleted: u64 = 0;
    loop {
        let affected = sqlx::query(
            r#"
            DELETE FROM session_timeline
            WHERE id IN (
                SELECT id FROM session_timeline
                WHERE created_at < now() - make_interval(days => $1)
                ORDER BY id
                LIMIT $2
            )
            "#,
        )
        .bind(days as i32)
        .bind(batch_size)
        .execute(db)
        .await?
        .rows_affected();

        total_deleted = total_deleted.saturating_add(affected);
        if (affected as i64) < batch_size {
            // Fewer than batch_size rows deleted — no more work to do.
            break;
        }
        // Yield between batches so cleanup doesn't starve other tokio tasks.
        tokio::task::yield_now().await;
    }
    Ok(total_deleted)
}

/// Timeline event row for LoopDetector warm-up.
#[derive(Debug)]
pub struct TimelineToolEvent {
    pub tool_name: String,
    pub success: bool,
    /// Hex of the loop hash (over `loop_detector_key` + args); None for legacy
    /// `tool_end` rows that predate the hash being persisted.
    pub args_hash: Option<String>,
}

/// Load tool_end events for a session to replay into LoopDetector (BUG-026).
/// The timeline payload for tool_end events contains: {"tool_call_id": "...", "tool_name": "...", "success": true/false, "args_hash": "..."}
///
/// H5 fix: bounded to the most recent `TOOL_EVENTS_WARMUP_LIMIT` rows. The
/// LoopDetector's `recent` deque is 64 entries wide, so anything older than
/// ~256 rows is discarded immediately — fetching all rows paid O(N) DB scan
/// + O(N) allocation per resume on long-lived sessions (cron / goal driver).
pub async fn load_tool_events(db: &PgPool, session_id: Uuid) -> Result<Vec<TimelineToolEvent>> {
    let rows = sqlx::query_as::<_, (String, Option<bool>, Option<String>)>(
        r#"
        WITH recent_rows AS (
            SELECT
                payload->>'tool_name' AS tool_name,
                (payload->>'success')::bool AS success,
                payload->>'args_hash' AS args_hash,
                created_at
            FROM session_timeline
            WHERE session_id = $1
              AND event_type = 'tool_end'
              AND payload->>'tool_name' IS NOT NULL
            ORDER BY created_at DESC
            LIMIT $2
        )
        SELECT tool_name, success, args_hash FROM recent_rows
        ORDER BY created_at ASC
        "#,
    )
    .bind(session_id)
    // 4× the LoopDetector `recent` window (64). Keeps hash-repeat + recent
    // error streak fully hydrated without paying for the entire history.
    .bind(TOOL_EVENTS_WARMUP_LIMIT as i64)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(name, success, args_hash)| TimelineToolEvent {
            tool_name: name,
            success: success.unwrap_or(true),
            args_hash,
        })
        .collect())
}

/// Returns true if the session already has at least one timeline row of
/// `event_type`. Used to rate-limit proactive side effects to once per session
/// (e.g. the SeekSupport owner message — see knowledge_extractor.rs).
pub async fn has_event_type(db: &PgPool, session_id: Uuid, event_type: &str) -> Result<bool> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM session_timeline WHERE session_id = $1 AND event_type = $2)",
    )
    .bind(session_id)
    .bind(event_type)
    .fetch_one(db)
    .await?;
    Ok(exists)
}

/// H5: maximum number of `tool_end` rows `load_tool_events` will hydrate the
/// LoopDetector with. See that function's doc comment for the rationale.
pub const TOOL_EVENTS_WARMUP_LIMIT: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn log_event_tx_updates_activity_at(pool: sqlx::PgPool) {
        let session_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, activity_at)
             VALUES ($1, 'a', 'u', 'web', 'running', NOW() - INTERVAL '5 minutes')"
        ).bind(session_id).execute(&pool).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        log_event_tx(&mut tx, session_id, "tool_start", None).await.unwrap();
        tx.commit().await.unwrap();

        let recent: bool = sqlx::query_scalar(
            "SELECT activity_at > NOW() - INTERVAL '5 seconds' FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert!(recent, "activity_at must be refreshed by log_event_tx");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn log_event_tx_debounce_skips_recent(pool: sqlx::PgPool) {
        let session_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, activity_at)
             VALUES ($1, 'a', 'u', 'web', 'running', NOW())"
        ).bind(session_id).execute(&pool).await.unwrap();

        let before: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT activity_at FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();

        // Sleep < 10s, then log — heartbeat should NOT bump activity_at.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut tx = pool.begin().await.unwrap();
        log_event_tx(&mut tx, session_id, "tool_start", None).await.unwrap();
        tx.commit().await.unwrap();

        let after: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT activity_at FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(before, after, "debounce must skip heartbeat update within 10s");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn log_event_tx_does_not_resurrect_terminal(pool: sqlx::PgPool) {
        let session_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, activity_at)
             VALUES ($1, 'a', 'u', 'web', 'done', NOW() - INTERVAL '1 hour')"
        ).bind(session_id).execute(&pool).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        log_event_tx(&mut tx, session_id, "tool_start", None).await.unwrap();
        tx.commit().await.unwrap();

        let still_old: bool = sqlx::query_scalar(
            "SELECT activity_at < NOW() - INTERVAL '50 minutes' FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert!(still_old, "terminal session must not heartbeat");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn log_event_tx_heartbeats_when_activity_at_is_null(pool: sqlx::PgPool) {
        let session_id = uuid::Uuid::new_v4();
        // Fresh session with activity_at NULL — the IS NULL branch of the
        // debounce predicate must allow the heartbeat.
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, activity_at)
             VALUES ($1, 'a', 'u', 'web', 'running', NULL)"
        ).bind(session_id).execute(&pool).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        log_event_tx(&mut tx, session_id, "tool_start", None).await.unwrap();
        tx.commit().await.unwrap();

        let now_set: bool = sqlx::query_scalar(
            "SELECT activity_at IS NOT NULL FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert!(now_set, "IS NULL branch of debounce predicate must allow heartbeat");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn m049_renames_session_events_to_session_timeline(pool: sqlx::PgPool) {
        // After all migrations run, the table must be `session_timeline` and the
        // old `session_events` name must not resolve.
        let exists_new: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = 'session_timeline')"
        ).fetch_one(&pool).await.unwrap();
        assert!(exists_new, "session_timeline table must exist after m049");

        let exists_old: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = 'session_events')"
        ).fetch_one(&pool).await.unwrap();
        assert!(!exists_old, "session_events table must be gone after m049");

        // Indexes renamed.
        let idx_session: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM pg_indexes \
             WHERE indexname = 'idx_session_timeline_session')"
        ).fetch_one(&pool).await.unwrap();
        assert!(idx_session, "idx_session_timeline_session must exist after m049");

        let idx_type: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM pg_indexes \
             WHERE indexname = 'idx_session_timeline_type')"
        ).fetch_one(&pool).await.unwrap();
        assert!(idx_type, "idx_session_timeline_type must exist after m049");
    }
}
