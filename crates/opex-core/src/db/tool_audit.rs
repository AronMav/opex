//! Tool execution audit trail — records every tool invocation.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Record a single tool execution (fire-and-forget via `tokio::spawn`).
#[allow(clippy::too_many_arguments)]
pub async fn record_tool_execution(
    db: &PgPool,
    agent_id: &str,
    session_id: Option<Uuid>,
    tool_name: &str,
    parameters: Option<&serde_json::Value>,
    status: &str,
    duration_ms: Option<i32>,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO audit_log (agent_id, session_id, tool_name, parameters, status, duration_ms, error) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(agent_id)
    .bind(session_id)
    .bind(tool_name)
    .bind(parameters)
    .bind(status)
    .bind(duration_ms)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Debug, FromRow, Serialize)]
pub struct ToolAuditEntry {
    pub id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub tool_name: String,
    pub parameters: Option<serde_json::Value>,
    pub status: String,
    pub duration_ms: Option<i32>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Query tool audit log with optional filters.
pub async fn query_tool_audit(
    db: &PgPool,
    agent_id: Option<&str>,
    tool_name: Option<&str>,
    days: u32,
    limit: i64,
) -> Result<Vec<ToolAuditEntry>> {
    let rows = sqlx::query_as::<_, ToolAuditEntry>(
        "SELECT id, agent_id, session_id, tool_name, parameters, status, duration_ms, error, created_at \
         FROM audit_log \
         WHERE ($1::TEXT IS NULL OR agent_id = $1) \
         AND ($2::TEXT IS NULL OR tool_name = $2) \
         AND created_at > now() - make_interval(days => $3) \
         ORDER BY created_at DESC \
         LIMIT $4",
    )
    .bind(agent_id)
    .bind(tool_name)
    .bind(days as i32)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Delete `audit_log` entries older than `retention_days`.
pub async fn cleanup_old_entries(db: &PgPool, retention_days: u32) -> Result<u64> {
    if retention_days == 0 {
        return Ok(0);
    }
    let result = sqlx::query(
        "DELETE FROM audit_log WHERE created_at < now() - make_interval(days => $1)",
    )
    .bind(retention_days as i32)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
