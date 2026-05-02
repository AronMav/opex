use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct CuratorRunRow {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i32>,
    pub triggered_by: String,
    pub phase1_transitions: i32,
    pub phase2_repairs: i32,
    pub phase3_commands: i32,
    pub skipped_reason: Option<String>,
    pub report_md: Option<String>,
    pub error: Option<String>,
}

pub async fn insert_run(db: &PgPool, triggered_by: &str) -> sqlx::Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO curator_runs (triggered_by) VALUES ($1) RETURNING id",
    )
    .bind(triggered_by)
    .fetch_one(db)
    .await
}

pub async fn finish_run(
    db: &PgPool,
    id: Uuid,
    phase1: i32,
    phase2: i32,
    phase3: i32,
    report_md: Option<&str>,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE curator_runs SET
           finished_at = NOW(),
           duration_ms = EXTRACT(EPOCH FROM (NOW() - started_at))::INT * 1000,
           phase1_transitions = $2,
           phase2_repairs = $3,
           phase3_commands = $4,
           report_md = $5,
           error = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(phase1)
    .bind(phase2)
    .bind(phase3)
    .bind(report_md)
    .bind(error)
    .fetch_optional(db)
    .await?;
    Ok(())
}

pub async fn skip_run(db: &PgPool, id: Uuid, reason: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE curator_runs SET finished_at = NOW(), duration_ms = 0, skipped_reason = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(reason)
    .fetch_optional(db)
    .await?;
    Ok(())
}

pub async fn list_runs(db: &PgPool, limit: i64) -> sqlx::Result<Vec<CuratorRunRow>> {
    sqlx::query_as::<_, CuratorRunRow>(
        "SELECT * FROM curator_runs ORDER BY started_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn get_run(db: &PgPool, id: Uuid) -> sqlx::Result<Option<CuratorRunRow>> {
    sqlx::query_as::<_, CuratorRunRow>("SELECT * FROM curator_runs WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn last_run(db: &PgPool) -> sqlx::Result<Option<CuratorRunRow>> {
    sqlx::query_as::<_, CuratorRunRow>(
        "SELECT * FROM curator_runs WHERE skipped_reason IS NULL ORDER BY started_at DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await
}
