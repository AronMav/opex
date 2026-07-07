//! Per-agent operator-configurable handler settings ("valves").
//!
//! The configurable fields are declared in each handler's `<config>` descriptor
//! block (parsed by toolgate, surfaced via `HandlerManifest.config`). The values
//! an operator sets in the tool-settings UI are stored here keyed by
//! `(handler_id, agent_name)` and injected as `ctx.config` when the handler runs
//! (async via `file_handler_worker`, sync via `files.rs`).

use sqlx::PgPool;

/// Saved config values for `(handler_id, agent_name)`, or `{}` when none set.
pub async fn get_config(
    db: &PgPool,
    handler_id: &str,
    agent_name: &str,
) -> sqlx::Result<serde_json::Value> {
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT config_values FROM handler_config WHERE handler_id = $1 AND agent_name = $2",
    )
    .bind(handler_id)
    .bind(agent_name)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| r.0).unwrap_or_else(|| serde_json::json!({})))
}

/// Upsert the config values for `(handler_id, agent_name)`.
pub async fn set_config(
    db: &PgPool,
    handler_id: &str,
    agent_name: &str,
    values: &serde_json::Value,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO handler_config (handler_id, agent_name, config_values, updated_at) \
         VALUES ($1, $2, $3, now()) \
         ON CONFLICT (handler_id, agent_name) \
         DO UPDATE SET config_values = EXCLUDED.config_values, updated_at = now()",
    )
    .bind(handler_id)
    .bind(agent_name)
    .bind(values)
    .execute(db)
    .await?;
    Ok(())
}
