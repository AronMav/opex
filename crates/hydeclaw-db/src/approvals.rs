//! Database operations for the tool-call approval system.

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Phase 63 DATA-04: structured error for [`resolve_approval_strict`].
///
/// `NotFound` and `AlreadyResolved` are surfaced explicitly so callers can
/// distinguish "I won the race" (`Ok(())`) from "someone else won" or
/// "the row is gone". `Db` wraps any underlying sqlx error.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("approval {id} not found")]
    NotFound { id: Uuid },
    #[error("approval {id} already resolved (status={status})")]
    AlreadyResolved { id: Uuid, status: String },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, FromRow, Clone)]
#[allow(dead_code)]
pub struct PendingApproval {
    pub id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub status: String,
    pub requested_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<String>,
    pub context: Option<serde_json::Value>,
}

/// Create a new pending approval request.
pub async fn create_approval(
    db: &PgPool,
    agent_id: &str,
    session_id: Option<Uuid>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    context: &serde_json::Value,
) -> Result<Uuid> {
    let row = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO pending_approvals (agent_id, session_id, tool_name, tool_args, context) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(agent_id)
    .bind(session_id)
    .bind(tool_name)
    .bind(tool_args)
    .bind(context)
    .fetch_one(db)
    .await?;

    Ok(row)
}

/// Phase 63 DATA-04: transactionally resolve an approval with row-level locking.
///
/// Flow:
///
/// ```text
///   BEGIN
///   SELECT status FROM pending_approvals WHERE id = $1 FOR UPDATE
///     - not found   → ROLLBACK, return ApprovalError::NotFound
///     - not pending → ROLLBACK, return ApprovalError::AlreadyResolved { status }
///     - pending     → UPDATE ..., COMMIT, return Ok(())
/// ```
///
/// Default isolation is `READ COMMITTED`; `FOR UPDATE` serialises competing
/// resolvers on the same PK. 100 concurrent callers produce exactly
/// `1 Ok(())` and `99 Err(AlreadyResolved)` — verified by
/// `integration_data_layer_approval.rs` and the strict block in
/// `integration_approval_race.rs`.
pub async fn resolve_approval_strict(
    db: &PgPool,
    id: Uuid,
    status: &str,
    resolved_by: &str,
) -> std::result::Result<(), ApprovalError> {
    let mut tx = db.begin().await?;

    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM pending_approvals WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;

    match row {
        None => {
            tx.rollback().await?;
            Err(ApprovalError::NotFound { id })
        }
        Some((current_status,)) if current_status != "pending" => {
            tx.rollback().await?;
            Err(ApprovalError::AlreadyResolved {
                id,
                status: current_status,
            })
        }
        Some(_) => {
            sqlx::query(
                "UPDATE pending_approvals SET status = $2, resolved_at = now(), resolved_by = $3 \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(status)
            .bind(resolved_by)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        }
    }
}

/// Get a specific approval by ID.
pub async fn get_approval(db: &PgPool, id: Uuid) -> Result<Option<PendingApproval>> {
    let row = sqlx::query_as::<_, PendingApproval>(
        "SELECT * FROM pending_approvals WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;

    Ok(row)
}

/// List pending approvals for an agent.
pub async fn list_pending(db: &PgPool, agent_id: &str) -> Result<Vec<PendingApproval>> {
    let rows = sqlx::query_as::<_, PendingApproval>(
        "SELECT * FROM pending_approvals WHERE agent_id = $1 AND status = 'pending' \
         ORDER BY requested_at DESC LIMIT 200",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;

    Ok(rows)
}

pub async fn list_all_pending(db: &PgPool) -> Result<Vec<PendingApproval>> {
    let rows = sqlx::query_as::<_, PendingApproval>(
        "SELECT * FROM pending_approvals WHERE status = 'pending' \
         ORDER BY requested_at DESC LIMIT 200",
    )
    .fetch_all(db)
    .await?;

    Ok(rows)
}

// ── Allowlist ────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AllowlistEntry {
    pub id: Uuid,
    pub agent_id: String,
    pub tool_pattern: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub created_by: Option<String>,
}

pub async fn check_allowlist(db: &PgPool, agent_id: &str, tool_name: &str) -> Result<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM approval_allowlist
         WHERE agent_id = $1 AND (tool_pattern = $2 OR ($2 LIKE REPLACE(tool_pattern, '*', '%')))",
    )
    .bind(agent_id)
    .bind(tool_name)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

pub async fn list_allowlist(db: &PgPool, agent_id: &str) -> Result<Vec<AllowlistEntry>> {
    sqlx::query_as::<_, AllowlistEntry>(
        "SELECT * FROM approval_allowlist WHERE agent_id = $1 ORDER BY created_at DESC LIMIT 200",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await
    .map_err(Into::into)
}

pub async fn add_to_allowlist(db: &PgPool, agent_id: &str, tool_pattern: &str) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO approval_allowlist (agent_id, tool_pattern) VALUES ($1, $2) RETURNING id",
    )
    .bind(agent_id)
    .bind(tool_pattern)
    .fetch_one(db)
    .await?;
    Ok(row.0)
}

pub async fn remove_from_allowlist(db: &PgPool, id: Uuid) -> Result<bool> {
    let res = sqlx::query("DELETE FROM approval_allowlist WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(res.rows_affected() > 0)
}
