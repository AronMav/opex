use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn get_compaction_state(db: &PgPool, session_id: Uuid) -> Result<Option<serde_json::Value>> {
    let row = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT compaction_state FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.flatten())
}

pub async fn set_compaction_state(
    db: &PgPool,
    session_id: Uuid,
    state: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "UPDATE sessions SET compaction_state = $1 WHERE id = $2",
    )
    .bind(state)
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}
