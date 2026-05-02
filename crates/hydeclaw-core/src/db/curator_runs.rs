use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Insert a new curator run record with status `running`. Returns the new run id.
pub async fn insert_run(db: &PgPool, trigger: &str) -> Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO curator_runs (trigger, status, started_at) \
         VALUES ($1, 'running', now()) RETURNING id",
    )
    .bind(trigger)
    .fetch_one(db)
    .await?;
    Ok(id)
}

/// Mark a run as skipped (never executed the pipeline).
pub async fn skip_run(db: &PgPool, run_id: Uuid, reason: &str) -> Result<()> {
    sqlx::query(
        "UPDATE curator_runs \
         SET status = 'skipped', skip_reason = $2, finished_at = now() \
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(reason)
    .execute(db)
    .await?;
    Ok(())
}

/// Finalize a run as done or error.
///
/// Pass `report_md = Some(...)` and `error = None` on success.
/// Pass `report_md = None` and `error = Some(...)` on failure.
pub async fn finish_run(
    db: &PgPool,
    run_id: Uuid,
    phase1: i32,
    phase2: i32,
    phase3: i32,
    report_md: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let status = if error.is_some() { "error" } else { "done" };
    sqlx::query(
        "UPDATE curator_runs \
         SET status = $2, phase1 = $3, phase2 = $4, phase3 = $5, \
             report_md = $6, error = $7, finished_at = now() \
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(status)
    .bind(phase1)
    .bind(phase2)
    .bind(phase3)
    .bind(report_md)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}
