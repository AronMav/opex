use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)] // FromRow-mapped from stream_jobs; some fields are read by the
                    // /api/chat/{id}/stream resume handler via field access elsewhere.
pub struct StreamJob {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub status: String,
    pub aggregated_text: String,
    pub tool_calls: serde_json::Value,
    pub error_text: Option<String>,
}

/// Create a new streaming job. Returns job ID.
pub async fn create_job(db: &PgPool, session_id: Uuid, agent_id: &str) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO stream_jobs (session_id, agent_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(session_id)
    .bind(agent_id)
    .fetch_one(db)
    .await
}

/// Set the full aggregated content (single write at finish).
pub async fn set_content(
    db: &PgPool,
    job_id: Uuid,
    text: &str,
    tools: &[serde_json::Value],
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE stream_jobs SET aggregated_text = $2, tool_calls = $3 WHERE id = $1",
    )
    .bind(job_id)
    .bind(text)
    .bind(serde_json::json!(tools))
    .execute(db)
    .await?;
    Ok(())
}

// add_tool_call removed — tool calls accumulated in-memory, written via set_content()

/// Mark job as finished.
pub async fn finish_job(db: &PgPool, job_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE stream_jobs SET status = 'finished', finished_at = now() WHERE id = $1",
    )
    .bind(job_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark job as error.
pub async fn error_job(db: &PgPool, job_id: Uuid, error: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE stream_jobs SET status = 'error', finished_at = now(), error_text = $2 WHERE id = $1",
    )
    .bind(job_id)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

/// Get most recent job for a session (only recent — within last 2 minutes).
/// Prevents stale finished jobs from triggering sync on old sessions.
pub async fn get_active_job(db: &PgPool, session_id: Uuid) -> sqlx::Result<Option<StreamJob>> {
    sqlx::query_as::<_, StreamJob>(
        "SELECT id, session_id, agent_id, status, aggregated_text, tool_calls, error_text \
         FROM stream_jobs WHERE session_id = $1 \
         AND created_at > now() - interval '2 minutes' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
}

/// True when a NEWER stream job exists for the same session than `job_id` —
/// i.e. this turn's stream has been superseded by a later `POST /api/chat` on
/// the same session (web supersede is same-session; see `register_with_token`).
///
/// Used by `SessionLifecycleGuard` to ownership-gate the terminal
/// `sessions.run_status` write: a superseded turn no longer owns the session
/// row (the newer turn re-claimed it `running`), so its `done/fail/interrupt`
/// (and the Drop backstop) must NOT flip the row terminal — doing so would
/// strand the newer, still-running turn. Discriminates purely on `created_at`
/// ordering, so it does not conflate supersede with a genuine stream `error`.
/// Fail-open on a missing/own row (returns `false`) so a normal single turn is
/// never wrongly suppressed.
pub async fn is_superseded(db: &PgPool, job_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS ( \
           SELECT 1 FROM stream_jobs later \
           JOIN stream_jobs mine ON mine.id = $1 \
           WHERE later.session_id = mine.session_id \
             AND later.created_at > mine.created_at )",
    )
    .bind(job_id)
    .fetch_one(db)
    .await
}

/// Delete old jobs.
///
/// Audit 2026-05-08: previously this only removed rows with `finished_at IS
/// NOT NULL`. If the process crashed mid-stream the row stayed at
/// `status='running' AND finished_at IS NULL` forever, growing the table
/// unboundedly. We now also reap rows that have been `'running'` for more
/// than an hour (with `finished_at` still NULL) — by then the original
/// process is long gone and the SSE resume window (2 minutes per
/// `get_active_job`) has expired.
pub async fn cleanup_old_jobs(db: &PgPool) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM stream_jobs WHERE \
            (finished_at IS NOT NULL AND finished_at < now() - interval '1 hour') \
            OR (finished_at IS NULL AND created_at < now() - interval '1 hour')",
    )
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
