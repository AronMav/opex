//! Per-chat voice-mode storage (table `channel_voice_modes`).

use anyhow::Result;
use sqlx::PgPool;

/// Current voice mode for a chat (`"on"` / `"off"`). Returns `"off"` when unset.
pub async fn get_voice_mode(db: &PgPool, channel: &str, chat_id: &str) -> Result<String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT mode FROM channel_voice_modes WHERE channel = $1 AND chat_id = $2",
    )
    .bind(channel)
    .bind(chat_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(m,)| m).unwrap_or_else(|| "off".to_string()))
}

/// Upsert the voice mode for a chat.
pub async fn set_voice_mode(db: &PgPool, channel: &str, chat_id: &str, mode: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO channel_voice_modes (channel, chat_id, mode)
         VALUES ($1, $2, $3)
         ON CONFLICT (channel, chat_id)
         DO UPDATE SET mode = EXCLUDED.mode, updated_at = now()",
    )
    .bind(channel)
    .bind(chat_id)
    .bind(mode)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn defaults_to_off_then_roundtrips(pool: PgPool) -> sqlx::Result<()> {
        assert_eq!(get_voice_mode(&pool, "telegram", "42").await.unwrap(), "off");
        set_voice_mode(&pool, "telegram", "42", "on").await.unwrap();
        assert_eq!(get_voice_mode(&pool, "telegram", "42").await.unwrap(), "on");
        set_voice_mode(&pool, "telegram", "42", "off").await.unwrap();
        assert_eq!(get_voice_mode(&pool, "telegram", "42").await.unwrap(), "off");
        Ok(())
    }
}
