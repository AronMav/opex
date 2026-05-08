use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Row};
use uuid::Uuid;

#[derive(Debug, FromRow, serde::Serialize)]
pub struct TaskRow {
    pub id: Uuid,
    pub agent_id: String,
    pub user_id: String,
    pub source: String,
    pub status: String,
    pub input: String,
    pub plan: Option<serde_json::Value>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// List tasks for an agent, most recent first.
pub async fn list_tasks(db: &PgPool, agent_id: &str, limit: i64) -> Result<Vec<TaskRow>> {
    let rows = sqlx::query_as::<_, TaskRow>(
        "SELECT id, agent_id, user_id, source, status, input, plan, result, error, created_at, updated_at \
         FROM tasks WHERE agent_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(agent_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Get a single task by ID.
pub async fn get_task(db: &PgPool, task_id: Uuid) -> Result<Option<TaskRow>> {
    let row = sqlx::query_as::<_, TaskRow>(
        "SELECT id, agent_id, user_id, source, status, input, plan, result, error, created_at, updated_at \
         FROM tasks WHERE id = $1",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Delete a task and its steps (cascade).
pub async fn delete_task(db: &PgPool, task_id: Uuid) -> Result<bool> {
    let result = sqlx::query("DELETE FROM tasks WHERE id = $1")
        .bind(task_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Create a new task.
pub async fn create_task(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    source: &str,
    input: &str,
) -> Result<Uuid> {
    let row = sqlx::query(
        "INSERT INTO tasks (agent_id, user_id, source, input) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(agent_id)
    .bind(user_id)
    .bind(source)
    .bind(input)
    .fetch_one(db)
    .await?;

    Ok(row.get("id"))
}

/// Update step status.
pub async fn update_step_status(
    db: &PgPool,
    step_id: Uuid,
    status: &str,
    output: Option<&serde_json::Value>,
) -> Result<()> {
    let now = chrono::Utc::now();
    match status {
        "running" => {
            sqlx::query(
                "UPDATE task_steps SET status = $1, started_at = $2 WHERE id = $3",
            )
            .bind(status)
            .bind(now)
            .bind(step_id)
            .execute(db)
            .await?;
        }
        "completed" | "failed" => {
            sqlx::query(
                "UPDATE task_steps SET status = $1, output = $2, completed_at = $3 WHERE id = $4",
            )
            .bind(status)
            .bind(output)
            .bind(now)
            .bind(step_id)
            .execute(db)
            .await?;
        }
        _ => {
            sqlx::query("UPDATE task_steps SET status = $1 WHERE id = $2")
                .bind(status)
                .bind(step_id)
                .execute(db)
                .await?;
        }
    }

    Ok(())
}

/// Process a MCP callback — update step status and optionally complete the task.
pub async fn update_step_from_callback(
    db: &PgPool,
    callback: &hydeclaw_types::McpCallback,
) -> Result<()> {
    let step_id = callback.step_id.unwrap_or(callback.task_id);

    match callback.status.as_str() {
        "completed" => {
            update_step_status(db, step_id, "completed", callback.result.as_ref()).await?;
        }
        "failed" => {
            let error_output = callback
                .error
                .as_ref()
                .map(|e| serde_json::json!({"error": e}));
            update_step_status(db, step_id, "failed", error_output.as_ref()).await?;
        }
        "progress" => {
            tracing::debug!(step_id = %step_id, "MCP progress update");
        }
        other => {
            tracing::warn!(status = %other, "unknown callback status");
        }
    }

    Ok(())
}

/// Load pending steps for a task, ordered by `step_order`.
pub async fn load_task_steps(
    db: &PgPool,
    task_id: Uuid,
) -> Result<Vec<TaskStepRow>> {
    let rows = sqlx::query_as::<_, TaskStepRow>(
        "SELECT id, task_id, step_order, mcp_name, action, params, status, output \
         FROM task_steps WHERE task_id = $1 ORDER BY step_order",
    )
    .bind(task_id)
    .fetch_all(db)
    .await?;

    Ok(rows)
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)] // Fields read via FromRow derive
pub struct TaskStepRow {
    pub id: Uuid,
    pub task_id: Uuid,
    pub step_order: i32,
    pub mcp_name: String,
    pub action: String,
    pub params: Option<serde_json::Value>,
    pub status: String,
    pub output: Option<serde_json::Value>,
}

