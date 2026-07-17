use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

// ── Row type ───────────────────────────────────────────────────────────────────

/// A single row from `curator_runs`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CuratorRun {
    pub id: Uuid,
    pub trigger: String,
    pub status: String,
    pub skip_reason: Option<String>,
    pub phase1: Option<i32>,
    pub phase2: Option<i32>,
    pub phase3: Option<i32>,
    pub report_md: Option<String>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub dry_run: bool,
}

// ── Read helpers ───────────────────────────────────────────────────────────────

/// Return the most recent run (by `started_at DESC`), or `None` if the table
/// is empty.
pub async fn last_run(db: &PgPool) -> Result<Option<CuratorRun>> {
    let row = sqlx::query_as::<_, CuratorRun>(
        "SELECT id, trigger, status, skip_reason, phase1, phase2, phase3, \
               report_md, error, started_at, finished_at, dry_run \
         FROM curator_runs \
         ORDER BY started_at DESC \
         LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Return the most recent `limit` runs ordered newest-first.
pub async fn list_runs(db: &PgPool, limit: i64) -> Result<Vec<CuratorRun>> {
    let rows = sqlx::query_as::<_, CuratorRun>(
        "SELECT id, trigger, status, skip_reason, phase1, phase2, phase3, \
               report_md, error, started_at, finished_at, dry_run \
         FROM curator_runs \
         ORDER BY started_at DESC \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Return a single run by `id`, or `None` if not found.
#[allow(dead_code)] // sole caller was the removed GET /api/curator/runs/{id} route.
pub async fn get_run(db: &PgPool, id: Uuid) -> Result<Option<CuratorRun>> {
    let row = sqlx::query_as::<_, CuratorRun>(
        "SELECT id, trigger, status, skip_reason, phase1, phase2, phase3, \
               report_md, error, started_at, finished_at, dry_run \
         FROM curator_runs \
         WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

// ── Write helpers ──────────────────────────────────────────────────────────────

/// Insert a new curator run record with status `running`. Returns the new run id.
pub async fn insert_run(db: &PgPool, trigger: &str, dry_run: bool) -> Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO curator_runs (trigger, status, started_at, dry_run) \
         VALUES ($1, 'running', now(), $2) RETURNING id",
    )
    .bind(trigger)
    .bind(dry_run)
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
#[allow(clippy::too_many_arguments)]
pub async fn finish_run(
    db: &PgPool,
    run_id: Uuid,
    phase1: i32,
    phase2: i32,
    phase3: i32,
    report_md: Option<&str>,
    error: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let status = if error.is_some() { "error" } else { "done" };
    sqlx::query(
        "UPDATE curator_runs \
         SET status = $2, phase1 = $3, phase2 = $4, phase3 = $5, \
             report_md = $6, error = $7, finished_at = now(), dry_run = $8 \
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(status)
    .bind(phase1)
    .bind(phase2)
    .bind(phase3)
    .bind(report_md)
    .bind(error)
    .bind(dry_run)
    .execute(db)
    .await?;
    Ok(())
}
