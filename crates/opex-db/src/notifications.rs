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
        ORDER BY created_at DESC, id DESC
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

/// Count currently-unread notifications. Used to build cross-tab read-sync
/// broadcast payloads with a server-authoritative unread count.
pub async fn count_unread(db: &PgPool) -> Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read = FALSE")
        .fetch_one(db)
        .await?;
    Ok(n)
}

/// List notifications strictly older than the `(created_at, id)` cursor,
/// newest-first. Returns (rows, `total_unread_count`). Powers history
/// pagination in the notification bell. Cursor is composite because `id` is a
/// UUID (not monotonic) — ties on `created_at` are broken by `id`.
pub async fn list_notifications_before(
    db: &PgPool,
    before: chrono::DateTime<chrono::Utc>,
    before_id: Uuid,
    limit: i64,
) -> Result<(Vec<Notification>, i64)> {
    let rows = sqlx::query_as::<_, Notification>(
        r"
        SELECT id, type AS notification_type, title, body, data, read, created_at
        FROM notifications
        WHERE (created_at, id) < ($1, $2)
        ORDER BY created_at DESC, id DESC
        LIMIT $3
        ",
    )
    .bind(before)
    .bind(before_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let unread: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read = FALSE")
        .fetch_one(db)
        .await?;

    Ok((rows, unread))
}

/// Mark the `tool_approval` notification for a given approval id as read.
/// Returns the notification id if an unread row was updated (so the caller can
/// broadcast a `notification_read` reconciliation event), else None.
pub async fn mark_tool_approval_read_by_approval_id(
    db: &PgPool,
    approval_id: &str,
) -> Result<Option<Uuid>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        r"
        UPDATE notifications
        SET read = TRUE
        WHERE type = 'tool_approval'
          AND read = FALSE
          AND data->>'approval_id' = $1
        RETURNING id
        ",
    )
    .bind(approval_id)
    .fetch_optional(db)
    .await?;
    Ok(id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn count_unread_counts_only_unread(pool: PgPool) -> Result<()> {
        create_notification(&pool, "agent_error", "a", "b", serde_json::json!({})).await?;
        let n2 = create_notification(&pool, "agent_error", "c", "d", serde_json::json!({})).await?;
        assert_eq!(count_unread(&pool).await?, 2);
        mark_read(&pool, n2.id).await?;
        assert_eq!(count_unread(&pool).await?, 1);
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mark_tool_approval_read_by_approval_id_marks_only_match(pool: PgPool) -> Result<()> {
        let n = create_notification(&pool, "tool_approval", "t", "b",
            serde_json::json!({"approval_id": "ap-123"})).await?;
        create_notification(&pool, "tool_approval", "t2", "b2",
            serde_json::json!({"approval_id": "other"})).await?;
        let marked = mark_tool_approval_read_by_approval_id(&pool, "ap-123").await?;
        assert_eq!(marked, Some(n.id));
        assert_eq!(count_unread(&pool).await?, 1); // only "other" remains unread
        // idempotent: second call finds nothing to update
        assert_eq!(mark_tool_approval_read_by_approval_id(&pool, "ap-123").await?, None);
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_notifications_before_paginates_older(pool: PgPool) -> Result<()> {
        create_notification(&pool, "agent_error", "a", "", serde_json::json!({})).await?;
        create_notification(&pool, "agent_error", "b", "", serde_json::json!({})).await?;
        create_notification(&pool, "agent_error", "c", "", serde_json::json!({})).await?;

        // Full newest-first list (a<b<c by created_at, so order is [c, b, a]).
        let (all, _) = list_notifications(&pool, 10, 0).await?;
        assert_eq!(all.len(), 3);

        // Cursor = newest row → expect the rest in the SAME order the full list has.
        let (older, unread) =
            list_notifications_before(&pool, all[0].created_at, all[0].id, 10).await?;
        assert_eq!(
            older.iter().map(|n| n.id).collect::<Vec<_>>(),
            all[1..].iter().map(|n| n.id).collect::<Vec<_>>(),
        );
        assert_eq!(unread, 3);

        // limit is honored: only the first older row.
        let (one, _) = list_notifications_before(&pool, all[0].created_at, all[0].id, 1).await?;
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, all[1].id);
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn pagination_does_not_skip_rows_with_equal_created_at(pool: PgPool) -> Result<()> {
        // Force three rows sharing the SAME created_at so ordering relies entirely
        // on the id tiebreak — the boundary case the composite cursor must handle.
        let ts = "2026-07-15T12:00:00Z";
        for i in 0..3u8 {
            sqlx::query(
                "INSERT INTO notifications (type, title, body, data, created_at)
                 VALUES ('agent_error', $1, '', '{}'::jsonb, $2::timestamptz)",
            )
            .bind(format!("row{i}"))
            .bind(ts)
            .execute(&pool)
            .await?;
        }
        // Walk the whole history one row at a time via the cursor; every row must
        // appear exactly once (no skip, no dup) — proves the total order matches.
        let mut seen = std::collections::HashSet::new();
        let (mut page, _) = list_notifications(&pool, 1, 0).await?;
        while let Some(row) = page.first().cloned() {
            assert!(seen.insert(row.id), "row returned twice: {}", row.id);
            let next = list_notifications_before(&pool, row.created_at, row.id, 1).await?;
            page = next.0;
        }
        assert_eq!(seen.len(), 3, "pagination skipped a tied row");
        Ok(())
    }
}
