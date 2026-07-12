use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct InfraDecision {
    pub id: Uuid,
    pub container: String,
    pub diagnosis: String,
    pub proposed_action: String,
    pub proposed_commands: serde_json::Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum InfraError {
    #[error("infra decision {id} not found")]
    NotFound { id: Uuid },
    #[error("infra decision {id} already resolved (status={status})")]
    AlreadyResolved { id: Uuid, status: String },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub async fn create(
    db: &PgPool,
    container: &str,
    diagnosis: &str,
    proposed_action: &str,
    proposed_commands: &serde_json::Value,
    status: &str,
    ttl_days: i64,
) -> Result<Uuid, sqlx::Error> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO infra_decisions \
           (container, diagnosis, proposed_action, proposed_commands, status, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + ($6 || ' days')::interval) RETURNING id",
    )
    .bind(container)
    .bind(diagnosis)
    .bind(proposed_action)
    .bind(proposed_commands)
    .bind(status)
    .bind(ttl_days.to_string())
    .fetch_one(db)
    .await?;
    Ok(id)
}

pub async fn get(db: &PgPool, id: Uuid) -> Result<Option<InfraDecision>, sqlx::Error> {
    sqlx::query_as::<_, InfraDecision>("SELECT * FROM infra_decisions WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Транзакционный resolve с FOR UPDATE. Отклоняет не-`pending`.
pub async fn resolve_strict(
    db: &PgPool,
    id: Uuid,
    status: &str,
    resolved_by: &str,
) -> Result<InfraDecision, InfraError> {
    let mut tx = db.begin().await?;
    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM infra_decisions WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    match row {
        None => {
            tx.rollback().await?;
            Err(InfraError::NotFound { id })
        }
        Some((s,)) if s != "pending" => {
            tx.rollback().await?;
            Err(InfraError::AlreadyResolved { id, status: s })
        }
        Some(_) => {
            let updated = sqlx::query_as::<_, InfraDecision>(
                "UPDATE infra_decisions SET status = $2, resolved_at = now(), resolved_by = $3 \
                 WHERE id = $1 RETURNING *",
            )
            .bind(id)
            .bind(status)
            .bind(resolved_by)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(updated)
        }
    }
}

/// Обновить статус завершённого исполнения (done/failed). Не транзакционно —
/// вызывается Opex после выполнения одобренного действия.
pub async fn mark_status(db: &PgPool, id: Uuid, status: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE infra_decisions SET status = $2, resolved_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .execute(db)
    .await?;
    Ok(())
}

/// Обновить содержимое pending-решения (diagnosis/action/commands), НЕ трогая статус.
/// COALESCE — обновляются только переданные (`Some`) поля. Вызывается Opex при
/// дополнении авто-созданного pending диагнозом/командами перед решением владельца.
pub async fn update_content(
    db: &PgPool,
    id: Uuid,
    diagnosis: Option<&str>,
    proposed_action: Option<&str>,
    proposed_commands: Option<&serde_json::Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE infra_decisions SET \
           diagnosis = COALESCE($2, diagnosis), \
           proposed_action = COALESCE($3, proposed_action), \
           proposed_commands = COALESCE($4, proposed_commands) \
         WHERE id = $1",
    )
    .bind(id)
    .bind(diagnosis)
    .bind(proposed_action)
    .bind(proposed_commands)
    .execute(db)
    .await?;
    Ok(())
}

/// Дебаунс: есть ли недавняя запись, подавляющая новый триггер по контейнеру.
pub async fn has_recent(
    db: &PgPool,
    container: &str,
    cooldown_hours: i64,
) -> Result<bool, sqlx::Error> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS ( \
           SELECT 1 FROM infra_decisions WHERE container = $1 AND ( \
             (status = 'pending' AND expires_at > now()) OR \
             (status <> 'expired' \
              AND created_at > now() - ($2 || ' hours')::interval) \
           ) )",
    )
    .bind(container)
    .bind(cooldown_hours.to_string())
    .fetch_one(db)
    .await?;
    Ok(exists)
}

/// Пометить просроченные pending/triaging как expired (ленивый TTL). Возвращает число строк.
pub async fn expire_stale(db: &PgPool) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE infra_decisions SET status = 'expired' \
         WHERE status IN ('pending', 'triaging') AND expires_at < now()",
    )
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}

pub async fn list(db: &PgPool, limit: i64) -> Result<Vec<InfraDecision>, sqlx::Error> {
    sqlx::query_as::<_, InfraDecision>(
        "SELECT * FROM infra_decisions ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await
}
