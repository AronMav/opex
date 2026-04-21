//! Session WAL (Write-Ahead Log) — journal table for session lifecycle events.
//!
//! During normal operation, session state transitions (running, `tool_start`, `tool_end`,
//! done, failed) are logged to `session_events`. On crash recovery, this WAL is read
//! to identify what was in-flight and reconstruct state cleanly — no synthetic
//! "[interrupted]" messages are injected.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Log a session lifecycle event to the WAL.
pub async fn log_event(
    db: &PgPool,
    session_id: Uuid,
    event_type: &str,
    payload: Option<&serde_json::Value>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_events (session_id, event_type, payload) VALUES ($1, $2, $3)",
    )
    .bind(session_id)
    .bind(event_type)
    .bind(payload)
    .execute(db)
    .await?;
    Ok(())
}

/// Delete WAL events older than `days` to prevent unbounded table growth.
///
/// Unbounded single-statement DELETE — acquires a lock across the full scan
/// and can bloat WAL. Retained as a thin wrapper for backward compatibility;
/// new call sites should use `prune_old_events_batched` (Phase 62 RES-03).
pub async fn prune_old_events(db: &PgPool, days: u32) -> Result<u64> {
    if days == 0 {
        return Ok(0);
    }
    let result = sqlx::query(
        "DELETE FROM session_events WHERE created_at < now() - make_interval(days => $1)",
    )
    .bind(days as i32)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Phase 62 RES-03: batched DELETE variant of `prune_old_events`.
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
            DELETE FROM session_events
            WHERE id IN (
                SELECT id FROM session_events
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

/// WAL event row for LoopDetector warm-up.
#[derive(Debug)]
pub struct WalToolEvent {
    pub tool_name: String,
    pub success: bool,
}

/// Load tool_end events for a session to replay into LoopDetector (BUG-026).
/// The WAL payload for tool_end events contains: {"tool_call_id": "...", "tool_name": "...", "success": true/false}
pub async fn load_tool_events(db: &PgPool, session_id: Uuid) -> Result<Vec<WalToolEvent>> {
    let rows = sqlx::query_as::<_, (String, Option<bool>)>(
        r#"
        SELECT
            payload->>'tool_name' AS tool_name,
            (payload->>'success')::bool AS success
        FROM session_events
        WHERE session_id = $1
          AND event_type = 'tool_end'
          AND payload->>'tool_name' IS NOT NULL
        ORDER BY created_at ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(name, success)| WalToolEvent {
            tool_name: name,
            success: success.unwrap_or(true),
        })
        .collect())
}
