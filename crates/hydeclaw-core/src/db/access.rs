use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// Check if a channel user is in the allowed list for an agent.
pub async fn is_user_allowed(
    db: &PgPool,
    agent_id: &str,
    channel_user_id: &str,
) -> Result<bool> {
    let row = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
            SELECT 1 FROM channel_allowed_users
            WHERE agent_id = $1 AND channel_user_id = $2
        )",
    )
    .bind(agent_id)
    .bind(channel_user_id)
    .fetch_one(db)
    .await?;
    Ok(row)
}

/// Add a user to the allowed list.
pub async fn add_allowed_user(
    db: &PgPool,
    agent_id: &str,
    channel_user_id: &str,
    display_name: Option<&str>,
    approved_by: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO channel_allowed_users
            (agent_id, channel_user_id, display_name, approved_by)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (agent_id, channel_user_id) DO NOTHING",
    )
    .bind(agent_id)
    .bind(channel_user_id)
    .bind(display_name)
    .bind(approved_by)
    .execute(db)
    .await?;
    Ok(())
}

/// Remove a user from the allowed list. Returns true if a row was deleted.
pub async fn remove_allowed_user(
    db: &PgPool,
    agent_id: &str,
    channel_user_id: &str,
) -> Result<bool> {
    let result = sqlx::query(
        "DELETE FROM channel_allowed_users
         WHERE agent_id = $1 AND channel_user_id = $2",
    )
    .bind(agent_id)
    .bind(channel_user_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// List all allowed users for an agent.
pub async fn list_allowed_users(
    db: &PgPool,
    agent_id: &str,
) -> Result<Vec<AllowedUser>> {
    let rows = sqlx::query_as::<_, AllowedUser>(
        "SELECT channel_user_id, display_name, approved_at
         FROM channel_allowed_users
         WHERE agent_id = $1
         ORDER BY approved_at",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

#[derive(Debug, sqlx::FromRow)]
pub struct AllowedUser {
    pub channel_user_id: String,
    pub display_name: Option<String>,
    pub approved_at: DateTime<Utc>,
}

// ── Pairing codes (persistent, survive restarts) ────────────────────────────

/// Store a pairing code in DB. Replaces any existing code for the same user atomically.
pub async fn store_pairing_code(
    db: &PgPool,
    agent_id: &str,
    code: &str,
    channel_user_id: &str,
    display_name: Option<&str>,
) -> Result<()> {
    // Atomic: delete old codes for this user + insert new one in a transaction
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM pairing_codes WHERE agent_id = $1 AND channel_user_id = $2")
        .bind(agent_id).bind(channel_user_id).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO pairing_codes (code, agent_id, channel_user_id, display_name)
         VALUES ($1, $2, $3, $4)"
    )
    .bind(code).bind(agent_id).bind(channel_user_id).bind(display_name)
    .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Get and remove a pairing code. Returns (`channel_user_id`, `display_name`, expired).
pub async fn take_pairing_code(
    db: &PgPool,
    agent_id: &str,
    code: &str,
) -> Result<Option<(String, Option<String>, bool)>> {
    let row = sqlx::query_as::<_, (String, Option<String>, DateTime<Utc>)>(
        "DELETE FROM pairing_codes WHERE agent_id = $1 AND code = $2
         RETURNING channel_user_id, display_name, created_at"
    )
    .bind(agent_id).bind(code)
    .fetch_optional(db).await?;
    match row {
        Some((user_id, name, created_at)) => {
            let expired = (Utc::now() - created_at).num_seconds() > 300;
            Ok(Some((user_id, name, expired)))
        }
        None => Ok(None),
    }
}

/// Remove a pairing code (reject).
pub async fn remove_pairing_code(db: &PgPool, agent_id: &str, code: &str) -> Result<bool> {
    let r = sqlx::query("DELETE FROM pairing_codes WHERE agent_id = $1 AND code = $2")
        .bind(agent_id).bind(code).execute(db).await?;
    Ok(r.rows_affected() > 0)
}

/// List pending pairing codes for an agent (not expired).
pub async fn list_pairing_codes(db: &PgPool, agent_id: &str) -> Result<Vec<PairingCode>> {
    let rows = sqlx::query_as::<_, PairingCode>(
        "SELECT code, channel_user_id, display_name, created_at FROM pairing_codes
         WHERE agent_id = $1 AND created_at > now() - interval '5 minutes'
         ORDER BY created_at"
    )
    .bind(agent_id).fetch_all(db).await?;
    // Cleanup expired
    if let Err(e) = sqlx::query("DELETE FROM pairing_codes WHERE created_at <= now() - interval '5 minutes'")
        .execute(db).await
    {
        tracing::warn!(error = %e, "pairing code cleanup failed; table may accumulate stale rows");
    }
    Ok(rows)
}

#[derive(Debug, sqlx::FromRow)]
pub struct PairingCode {
    pub code: String,
    pub channel_user_id: String,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
}
