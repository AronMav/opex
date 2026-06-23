use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// One row from the notifications table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct Notification {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub notification_type: String,
    pub title: String,
    pub body: String,
    #[cfg_attr(feature = "ts-gen", ts(type = "Record<string, unknown>"))]
    pub data: serde_json::Value,
    pub read: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Response shape for GET /api/notifications.
#[derive(Debug, serde::Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct NotificationsResponseDto {
    pub items: Vec<Notification>,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub unread_count: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub limit: i64,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub offset: i64,
}

/// Insert a new notification. Returns the inserted row (with generated id and `created_at`).
pub async fn create_notification(
    db: &PgPool,
    notification_type: &str,
    title: &str,
    body: &str,
    data: serde_json::Value,
) -> Result<Notification> {
    let row = sqlx::query_as::<_, Notification>(
        r"
        INSERT INTO notifications (type, title, body, data)
        VALUES ($1, $2, $3, $4)
        RETURNING id, type AS notification_type, title, body, data, read, created_at
        ",
    )
    .bind(notification_type)
    .bind(title)
    .bind(body)
    .bind(data)
    .fetch_one(db)
    .await?;
    Ok(row)
}

/// List notifications newest-first with pagination.
/// Returns (rows, `total_unread_count`).
pub async fn list_notifications(
    db: &PgPool,
    limit: i64,
    offset: i64,
) -> Result<(Vec<Notification>, i64)> {
    let rows = sqlx::query_as::<_, Notification>(
        r"
        SELECT id, type AS notification_type, title, body, data, read, created_at
        FROM notifications
        ORDER BY created_at DESC
        LIMIT $1 OFFSET $2
        ",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await?;

    let unread: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read = FALSE")
        .fetch_one(db)
        .await?;

    Ok((rows, unread))
}

/// Mark a single notification as read by id. Returns true if a row was updated.
pub async fn mark_read(db: &PgPool, id: Uuid) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE notifications SET read = TRUE WHERE id = $1 AND read = FALSE",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Mark ALL notifications as read. Returns the count of updated rows.
pub async fn mark_all_read(db: &PgPool) -> Result<u64> {
    let result = sqlx::query("UPDATE notifications SET read = TRUE WHERE read = FALSE")
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

/// Delete notifications older than 30 days.
pub async fn cleanup_old_notifications(db: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM notifications WHERE created_at < NOW() - INTERVAL '30 days'"
    )
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
