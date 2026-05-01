use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct SkillRepairRow {
    pub id: Uuid,
    pub skill_name: String,
    pub agent_name: String,
    pub kind: String,
    pub diagnosis: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution_note: Option<String>,
}

pub async fn enqueue(
    db: &PgPool,
    skill_name: &str,
    agent_name: &str,
    kind: &str,
    diagnosis: &str,
) -> sqlx::Result<Uuid> {
    let row = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO pending_skill_repairs (skill_name, agent_name, kind, diagnosis)
         VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(skill_name)
    .bind(agent_name)
    .bind(kind)
    .bind(diagnosis)
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn list(
    db: &PgPool,
    status_filter: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<SkillRepairRow>> {
    match status_filter {
        Some(s) => {
            sqlx::query_as::<_, SkillRepairRow>(
                "SELECT * FROM pending_skill_repairs
                 WHERE status = $1
                 ORDER BY created_at DESC LIMIT $2",
            )
            .bind(s)
            .bind(limit)
            .fetch_all(db)
            .await
        }
        None => {
            sqlx::query_as::<_, SkillRepairRow>(
                "SELECT * FROM pending_skill_repairs
                 ORDER BY created_at DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(db)
            .await
        }
    }
}

pub async fn resolve(
    db: &PgPool,
    id: Uuid,
    status: &str,
    note: Option<&str>,
) -> sqlx::Result<bool> {
    let rows = sqlx::query(
        "UPDATE pending_skill_repairs
         SET status = $1, resolved_at = now(), resolution_note = $2
         WHERE id = $3 AND status IN ('pending','in_progress')",
    )
    .bind(status)
    .bind(note)
    .bind(id)
    .execute(db)
    .await?
    .rows_affected();
    Ok(rows > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_repair_row_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SkillRepairRow>();
    }
}
