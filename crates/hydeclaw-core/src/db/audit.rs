//! Structured audit logging for security-relevant events.

/// Well-known audit event types.
pub mod event_types {
    pub const APPROVAL_REQUESTED: &str = "approval_requested";
    pub const APPROVAL_RESOLVED: &str = "approval_resolved";
    #[allow(dead_code)]
    pub const PROMPT_INJECTION: &str = "prompt_injection_detected";
    pub const COMPACTION: &str = "compaction";
    // Agent lifecycle
    pub const AGENT_CREATED: &str = "agent_created";
    pub const AGENT_UPDATED: &str = "agent_updated";
    pub const AGENT_DELETED: &str = "agent_deleted";
    // Secrets
    pub const SECRET_CREATED: &str = "secret_created";
    pub const SECRET_DELETED: &str = "secret_deleted";
    pub const SECRET_REVEALED: &str = "secret_revealed";
    // Config
    pub const CONFIG_UPDATED: &str = "config_updated";
    // Tools
    pub const TOOL_VERIFIED: &str = "tool_verified";
    pub const TOOL_DISABLED: &str = "tool_disabled";
    pub const TOOL_ENABLED: &str = "tool_enabled";
    // Access control
    pub const ACCESS_APPROVED: &str = "access_approved";
    pub const ACCESS_REJECTED: &str = "access_rejected";
    // Memory
    pub const MEMORY_DELETED: &str = "memory_deleted";
    pub const MEMORY_PINNED: &str = "memory_pinned";
    // Rate limiting (reserved for future per-event logging)
    #[allow(dead_code)]
    pub const RATE_LIMITED: &str = "rate_limited";
}

/// Fire-and-forget audit log helper. Spawns a background task.
pub fn audit_spawn(db: PgPool, agent_id: String, event_type: &'static str, actor: Option<String>, details: serde_json::Value) {
    tokio::spawn(async move {
        if let Err(e) = record_event(&db, &agent_id, event_type, actor.as_deref(), &details).await {
            tracing::error!(error = %e, "audit event lost");
        }
    });
}

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, FromRow, Serialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub agent_id: String,
    pub event_type: String,
    pub actor: Option<String>,
    pub details: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Record an audit event. Intended to be called via `tokio::spawn` (fire-and-forget).
pub async fn record_event(
    db: &PgPool,
    agent_id: &str,
    event_type: &str,
    actor: Option<&str>,
    details: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_events (agent_id, event_type, actor, details) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(agent_id)
    .bind(event_type)
    .bind(actor)
    .bind(details)
    .execute(db)
    .await?;
    Ok(())
}

/// Query audit events with optional filters.
pub async fn query_events(
    db: &PgPool,
    agent_id: Option<&str>,
    event_type: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<AuditEvent>> {
    let rows = sqlx::query_as::<_, AuditEvent>(
        "SELECT id, agent_id, event_type, actor, details, created_at \
         FROM audit_events \
         WHERE ($1::TEXT IS NULL OR agent_id = $1) \
         AND ($2::TEXT IS NULL OR event_type = $2) \
         ORDER BY created_at DESC \
         LIMIT $3 OFFSET $4",
    )
    .bind(agent_id)
    .bind(event_type)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Delete audit events older than `retention_days`.
pub async fn cleanup_old_events(db: &PgPool, retention_days: u32) -> Result<u64> {
    if retention_days == 0 {
        return Ok(0);
    }
    let result = sqlx::query(
        "DELETE FROM audit_events WHERE created_at < now() - make_interval(days => $1)",
    )
    .bind(retention_days as i32)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::event_types;
    use std::collections::HashSet;

    fn all_constants() -> Vec<&'static str> {
        vec![
            event_types::APPROVAL_REQUESTED,
            event_types::APPROVAL_RESOLVED,
            event_types::PROMPT_INJECTION,
            event_types::COMPACTION,
            event_types::AGENT_CREATED,
            event_types::AGENT_UPDATED,
            event_types::AGENT_DELETED,
            event_types::SECRET_CREATED,
            event_types::SECRET_DELETED,
            event_types::SECRET_REVEALED,
            event_types::CONFIG_UPDATED,
            event_types::TOOL_VERIFIED,
            event_types::TOOL_DISABLED,
            event_types::TOOL_ENABLED,
            event_types::ACCESS_APPROVED,
            event_types::ACCESS_REJECTED,
            event_types::MEMORY_DELETED,
            event_types::MEMORY_PINNED,
            event_types::RATE_LIMITED,
        ]
    }

    #[test]
    fn all_constants_non_empty() {
        for c in all_constants() {
            assert!(!c.is_empty(), "constant is empty: {:?}", c);
        }
    }

    #[test]
    fn all_constants_unique() {
        let constants = all_constants();
        let set: HashSet<&str> = constants.iter().copied().collect();
        assert_eq!(
            set.len(),
            constants.len(),
            "duplicate event type constants detected"
        );
    }
}
