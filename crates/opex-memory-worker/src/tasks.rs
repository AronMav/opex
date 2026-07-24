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
    pub attempts: i32,
}

/// Max retries before a poison task is moved to terminal status `'dead'`
/// (F-03 — previously `claim_next` retried failed tasks forever, starving the
/// queue). 5 gives a genuine transient-flake budget while bounding poison loops.
const MAX_ATTEMPTS: i32 = 5;

/// Claim next pending task atomically (FOR UPDATE SKIP LOCKED).
/// When no pending tasks remain, retries failed tasks — BUT only those below
/// `MAX_ATTEMPTS`; exhausted tasks flip to terminal status `'dead'` so they stop
/// being reclaimed (F-03).
pub async fn claim_next(db: &PgPool) -> anyhow::Result<Option<MemoryTask>> {
    // Try pending first (bump attempts when a (re)claim starts processing).
    let task = sqlx::query_as::<_, MemoryTask>(
        "UPDATE memory_tasks
         SET status = 'processing', started_at = now(), attempts = attempts + 1
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

    // No pending — retire poison tasks (attempts exhausted) to terminal 'dead',
    // then retry the oldest retryable failed task.
    let retired = sqlx::query(
        "UPDATE memory_tasks SET status = 'dead'
         WHERE status = 'failed' AND attempts >= $1::int",
    )
    .bind(MAX_ATTEMPTS)
    .execute(db)
    .await?;
    if retired.rows_affected() > 0 {
        tracing::warn!(count = retired.rows_affected(), max_attempts = MAX_ATTEMPTS,
            "retired poison memory_tasks to 'dead' (exceeded retry budget)");
    }

    let retry = sqlx::query_as::<_, MemoryTask>(
        "UPDATE memory_tasks
         SET status = 'processing', started_at = now(), error = NULL, attempts = attempts + 1
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
        tracing::info!(id = %t.id, task_type = %t.task_type, attempts = t.attempts, "retrying failed task");
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
