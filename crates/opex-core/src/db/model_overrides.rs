//! Persistence for the per-agent `/model` runtime override
//! (table `agent_model_overrides`, migration 073). See T15 triage: the
//! override previously lived only in an in-memory `RwLock` on the agent's
//! provider and was lost on every restart.

use anyhow::Result;
use sqlx::PgPool;

/// Read the persisted override for an agent, if any.
pub async fn get(db: &PgPool, agent_name: &str) -> Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT model FROM agent_model_overrides WHERE agent_name = $1")
            .bind(agent_name)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(m,)| m))
}

/// Set (upsert) or clear (delete) the persisted override for an agent.
/// `model = None` clears any existing override row.
pub async fn set(db: &PgPool, agent_name: &str, model: Option<&str>) -> Result<()> {
    match model {
        Some(m) => {
            sqlx::query(
                "INSERT INTO agent_model_overrides (agent_name, model, updated_at)
                 VALUES ($1, $2, now())
                 ON CONFLICT (agent_name) DO UPDATE SET model = EXCLUDED.model, updated_at = now()",
            )
            .bind(agent_name)
            .bind(m)
            .execute(db)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM agent_model_overrides WHERE agent_name = $1")
                .bind(agent_name)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn set_get_clear_roundtrip(pool: PgPool) -> sqlx::Result<()> {
        assert_eq!(get(&pool, "Agent").await.unwrap(), None);

        set(&pool, "Agent", Some("gpt-5")).await.unwrap();
        assert_eq!(get(&pool, "Agent").await.unwrap(), Some("gpt-5".to_string()));

        // Upsert overwrites.
        set(&pool, "Agent", Some("claude-opus")).await.unwrap();
        assert_eq!(get(&pool, "Agent").await.unwrap(), Some("claude-opus".to_string()));

        // Clearing deletes the row entirely.
        set(&pool, "Agent", None).await.unwrap();
        assert_eq!(get(&pool, "Agent").await.unwrap(), None);
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn overrides_are_isolated_per_agent(pool: PgPool) -> sqlx::Result<()> {
        set(&pool, "AgentA", Some("model-a")).await.unwrap();
        set(&pool, "AgentB", Some("model-b")).await.unwrap();
        assert_eq!(get(&pool, "AgentA").await.unwrap(), Some("model-a".to_string()));
        assert_eq!(get(&pool, "AgentB").await.unwrap(), Some("model-b".to_string()));

        set(&pool, "AgentA", None).await.unwrap();
        assert_eq!(get(&pool, "AgentA").await.unwrap(), None);
        assert_eq!(get(&pool, "AgentB").await.unwrap(), Some("model-b".to_string()), "clearing one agent must not affect another");
        Ok(())
    }
}
