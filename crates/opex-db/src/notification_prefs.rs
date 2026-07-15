use anyhow::Result;
use sqlx::PgPool;

/// One per-type notification preference row (global; single-operator).
/// Absent row = defaults (muted=false, sound=true).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct NotificationPref {
    #[serde(rename = "type")]
    pub notification_type: String,
    pub muted: bool,
    pub sound: bool,
}

/// All configured preference rows, ordered by type. Types with no row use
/// the defaults (the caller/UI fills them in).
pub async fn list_prefs(db: &PgPool) -> Result<Vec<NotificationPref>> {
    let rows = sqlx::query_as::<_, NotificationPref>(
        "SELECT notification_type, muted, sound FROM notification_prefs ORDER BY notification_type",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Whether a given type is muted. Absent row → false (not muted).
pub async fn is_muted(db: &PgPool, notification_type: &str) -> Result<bool> {
    let muted: Option<bool> =
        sqlx::query_scalar("SELECT muted FROM notification_prefs WHERE notification_type = $1")
            .bind(notification_type)
            .fetch_optional(db)
            .await?;
    Ok(muted.unwrap_or(false))
}

/// Insert or update a preference row (UPSERT on `notification_type`).
pub async fn upsert_pref(
    db: &PgPool,
    notification_type: &str,
    muted: bool,
    sound: bool,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO notification_prefs (notification_type, muted, sound)
         VALUES ($1, $2, $3)
         ON CONFLICT (notification_type)
         DO UPDATE SET muted = EXCLUDED.muted, sound = EXCLUDED.sound, updated_at = now()",
    )
    .bind(notification_type)
    .bind(muted)
    .bind(sound)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn prefs_upsert_and_read(pool: PgPool) -> Result<()> {
        // Default: absent row → not muted.
        assert!(!is_muted(&pool, "agent_error").await?);

        upsert_pref(&pool, "agent_error", true, false).await?;
        assert!(is_muted(&pool, "agent_error").await?);

        let all = list_prefs(&pool).await?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].notification_type, "agent_error");
        assert!(all[0].muted && !all[0].sound);

        // Upsert again flips the values (ON CONFLICT update).
        upsert_pref(&pool, "agent_error", false, true).await?;
        assert!(!is_muted(&pool, "agent_error").await?);
        let all2 = list_prefs(&pool).await?;
        assert_eq!(all2.len(), 1);
        assert!(!all2[0].muted && all2[0].sound);
        Ok(())
    }
}
