//! Pending messages for guaranteed delivery.
//!
//! When the channel adapter disconnects during processing, the response is saved
//! here and delivered when the adapter reconnects.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug)]
#[allow(dead_code)] // DB row mapped from pending_messages; some fields are serialized
                    // into JSON responses but not read from the Rust struct.
pub struct PendingMessage {
    pub id: Uuid,
    pub agent_id: String,
    pub request_id: String,
    pub channel: String,
    pub message_type: String,
    pub text: String,
}

/// Save an undelivered response for later delivery.
pub async fn save_pending(
    db: &PgPool,
    agent_id: &str,
    request_id: &str,
    channel: &str,
    message_type: &str,
    text: &str,
) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO pending_messages (agent_id, request_id, channel, message_type, text)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(agent_id)
    .bind(request_id)
    .bind(channel)
    .bind(message_type)
    .bind(text)
    .fetch_one(db)
    .await?;
    Ok(row.0)
}

/// Atomically fetch and delete all pending messages for an agent.
pub async fn take_pending(db: &PgPool, agent_id: &str) -> Result<Vec<PendingMessage>> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, String, String)>(
        "WITH taken AS (
            DELETE FROM pending_messages
            WHERE agent_id = $1
            RETURNING id, agent_id, request_id, channel, message_type, text, created_at
        )
        SELECT id, agent_id, request_id, channel, message_type, text FROM taken
        ORDER BY created_at ASC",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, agent_id, request_id, channel, message_type, text)| PendingMessage {
            id,
            agent_id,
            request_id,
            channel,
            message_type,
            text,
        })
        .collect())
}

/// Delete pending messages older than the given number of hours.
pub async fn cleanup_old(db: &PgPool, max_age_hours: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM pending_messages WHERE created_at < now() - make_interval(hours => $1)",
    )
    .bind(max_age_hours as i32)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
