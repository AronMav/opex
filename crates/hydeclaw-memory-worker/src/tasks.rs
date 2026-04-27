use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct MemoryTask {
    pub id: Uuid,
    pub task_type: String,
    pub status: String,
    pub params: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Claim next pending task atomically (FOR UPDATE SKIP LOCKED).
/// When no pending tasks remain, retries failed tasks automatically.
pub async fn claim_next(db: &PgPool) -> anyhow::Result<Option<MemoryTask>> {
    // Try pending first
    let task = sqlx::query_as::<_, MemoryTask>(
        "UPDATE memory_tasks
         SET status = 'processing', started_at = now()
         WHERE id = (
             SELECT id FROM memory_tasks
             WHERE status = 'pending'
             ORDER BY created_at LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING *",
    )
    .fetch_optional(db)
    .await?;

    if task.is_some() {
        return Ok(task);
    }

    // No pending — retry oldest failed task
    let retry = sqlx::query_as::<_, MemoryTask>(
        "UPDATE memory_tasks
         SET status = 'processing', started_at = now(), error = NULL
         WHERE id = (
             SELECT id FROM memory_tasks
             WHERE status = 'failed'
             ORDER BY completed_at LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING *",
    )
    .fetch_optional(db)
    .await?;

    if let Some(ref t) = retry {
        tracing::info!(id = %t.id, task_type = %t.task_type, "retrying failed task");
    }

    Ok(retry)
}

/// Mark task as done with result.
pub async fn complete(db: &PgPool, id: Uuid, result: serde_json::Value) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE memory_tasks SET status = 'done', result = $2, completed_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(result)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark task as failed.
pub async fn fail(db: &PgPool, id: Uuid, error: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE memory_tasks SET status = 'failed', error = $2, completed_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

/// Recover stuck 'processing' tasks on startup.
/// Tasks left in 'processing' after a worker crash are reset to 'pending'.
pub async fn recover_stuck(db: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "UPDATE memory_tasks SET status = 'pending', started_at = NULL
         WHERE status = 'processing'",
    )
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
