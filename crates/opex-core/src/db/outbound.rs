//! Persistent outbound delivery queue for channel actions.
//!
//! Actions are persisted before sending over WebSocket to the adapter.
//! On adapter reconnect, unacked actions are replayed automatically.

use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Enqueue a channel action for delivery. Returns the queue entry ID.
/// Deduplicates: skips if identical action was enqueued in the last 30 seconds.
pub async fn enqueue_action(
    db: &PgPool,
    agent_id: &str,
    channel: &str,
    action_name: &str,
    payload: &Value,
) -> Result<Uuid> {
    // First try to find an existing duplicate atomically.
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM outbound_queue
         WHERE agent_id = $1 AND channel = $2 AND action_name = $3
           AND payload = $4 AND created_at > NOW() - INTERVAL '30 seconds'
         LIMIT 1",
    )
    .bind(agent_id)
    .bind(channel)
    .bind(action_name)
    .bind(payload)
    .fetch_optional(db)
    .await?;

    if let Some((existing_id,)) = existing {
        tracing::debug!(id = %existing_id, action = %action_name, "dedup: skipping duplicate outbound action");
        return Ok(existing_id);
    }

    // Atomic insert: only inserts if no duplicate arrived between the SELECT above and now.
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO outbound_queue (agent_id, channel, action_name, payload)
         SELECT $1, $2, $3, $4
         WHERE NOT EXISTS (
             SELECT 1 FROM outbound_queue
             WHERE agent_id = $1 AND channel = $2 AND action_name = $3
               AND payload = $4 AND created_at > NOW() - INTERVAL '30 seconds'
         )
         RETURNING id",
    )
    .bind(agent_id)
    .bind(channel)
    .bind(action_name)
    .bind(payload)
    .fetch_optional(db)
    .await?;

    if let Some((id,)) = row {
        return Ok(id);
    }

    // A concurrent insert won the race — fetch the winner.
    let (winner_id,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM outbound_queue
         WHERE agent_id = $1 AND channel = $2 AND action_name = $3
           AND payload = $4 AND created_at > NOW() - INTERVAL '30 seconds'
         ORDER BY created_at ASC
         LIMIT 1",
    )
    .bind(agent_id)
    .bind(channel)
    .bind(action_name)
    .bind(payload)
    .fetch_one(db)
    .await?;
    tracing::debug!(id = %winner_id, action = %action_name, "dedup: concurrent insert, returning winner");
    Ok(winner_id)
}

/// Mark an action as sent (WebSocket send succeeded).
pub async fn mark_sent(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE outbound_queue SET status = 'sent', sent_at = NOW(), attempts = attempts + 1 WHERE id = $1",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark an action as acknowledged (adapter confirmed delivery).
pub async fn mark_acked(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE outbound_queue SET status = 'acked', acked_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Mark an action as failed (adapter reported error).
pub async fn mark_failed(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE outbound_queue SET status = 'failed' WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Get pending/sent actions for a channel that haven't exceeded retry limit.
/// Actions in 'sent' state older than 1 hour are considered stale and included.
/// Returns (id, `agent_id`, `action_name`, payload).
pub async fn get_pending(
    db: &PgPool,
    channel: &str,
    limit: i64,
) -> Result<Vec<(Uuid, String, String, Value)>> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, Value)>(
        "SELECT id, agent_id, action_name, payload FROM outbound_queue
         WHERE channel = $1
           AND attempts < 3
           AND (status = 'pending' OR (status = 'sent' AND sent_at < NOW() - INTERVAL '1 hour'))
         ORDER BY created_at ASC
         LIMIT $2",
    )
    .bind(channel)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Delete acked actions older than N days. Returns number of deleted rows.
pub async fn cleanup_old(db: &PgPool, days: i32) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM outbound_queue WHERE status = 'acked' AND acked_at < NOW() - make_interval(days => $1)",
    )
    .bind(days)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests require a running PostgreSQL instance.
    // Run with: cargo test outbound -- --nocapture

    #[tokio::test]
    async fn test_outbound_queue_lifecycle() {
        let db_url = match std::env::var("DATABASE_URL") {
            Ok(url) => url,
            Err(_) => {
                eprintln!("DATABASE_URL not set, skipping outbound queue test");
                return;
            }
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&db_url)
            .await
            .expect("failed to connect to test database");

        // Run migrations
        sqlx::migrate::Migrator::new(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../migrations")))
            .await
            .expect("failed to load migrations")
            .run(&pool)
            .await
            .expect("failed to run migrations");

        let payload = serde_json::json!({"text": "hello", "chat_id": 123});

        // Enqueue
        let id = enqueue_action(&pool, "test_agent", "telegram", "reply", &payload)
            .await
            .expect("enqueue failed");

        // Get pending
        let pending = get_pending(&pool, "telegram", 10).await.expect("get_pending failed");
        assert!(pending.iter().any(|(pid, _, _, _)| *pid == id), "enqueued action should be pending");

        // Mark sent
        mark_sent(&pool, id).await.expect("mark_sent failed");

        // Should not appear in pending (sent < 1 hour ago)
        let pending = get_pending(&pool, "telegram", 10).await.expect("get_pending failed");
        assert!(!pending.iter().any(|(pid, _, _, _)| *pid == id), "recently sent action should not be pending");

        // Mark acked
        mark_acked(&pool, id).await.expect("mark_acked failed");

        // Cleanup (should not delete — just acked)
        let deleted = cleanup_old(&pool, 7).await.expect("cleanup failed");
        // May or may not delete depending on timing, just check it doesn't error

        // Clean up test data
        sqlx::query("DELETE FROM outbound_queue WHERE agent_id = 'test_agent'")
            .execute(&pool)
            .await
            .ok();

        eprintln!("outbound queue lifecycle test passed (deleted={deleted})");
    }
}
