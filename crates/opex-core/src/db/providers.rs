use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderRow {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]
    pub category: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub enabled: bool,
    pub options: Value,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProvider {
    pub name: String,
    #[serde(rename = "type")]
    pub category: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub enabled: Option<bool>,
    pub options: Option<Value>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProvider {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub category: Option<String>,
    pub provider_type: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub enabled: Option<bool>,
    pub options: Option<Value>,
    pub notes: Option<String>,
}

// ── Provider Active (capability → provider) ─────────────────────────────────

/// Capability key for dedicated context-compaction LLM (cheap model for session compaction).
// Compaction provider now resolved from the profile `compaction` slot
// (profile_resolver); this legacy provider_active key is retained for the
// migration/back-compat surface.
#[allow(dead_code)]
pub const CAPABILITY_COMPACTION: &str = "compaction";

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderActiveRow {
    pub capability: String,
    pub provider_name: String,
    pub priority: i32,
}

// ── CRUD ─────────────────────────────────────────────────────────────────────

pub async fn list_providers(db: &PgPool) -> sqlx::Result<Vec<ProviderRow>> {
    sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers ORDER BY type, name")
        .fetch_all(db)
        .await
}

pub async fn list_providers_by_type(
    db: &PgPool,
    category: &str,
) -> sqlx::Result<Vec<ProviderRow>> {
    sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers WHERE type = $1 ORDER BY name")
        .bind(category)
        .fetch_all(db)
        .await
}

pub async fn get_provider(db: &PgPool, id: Uuid) -> sqlx::Result<Option<ProviderRow>> {
    sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn get_provider_by_name(db: &PgPool, name: &str) -> sqlx::Result<Option<ProviderRow>> {
    sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers WHERE name = $1")
        .bind(name)
        .fetch_optional(db)
        .await
}

pub async fn create_provider(db: &PgPool, input: CreateProvider) -> sqlx::Result<ProviderRow> {
    sqlx::query_as::<_, ProviderRow>(
        r"INSERT INTO providers
               (name, type, provider_type, base_url, default_model, enabled, options, notes)
           VALUES ($1, $2, $3, $4, $5, COALESCE($6, TRUE), COALESCE($7, '{}'), $8)
           RETURNING *",
    )
    .bind(input.name)
    .bind(input.category)
    .bind(input.provider_type)
    .bind(input.base_url)
    .bind(input.default_model)
    .bind(input.enabled)
    .bind(input.options)
    .bind(input.notes)
    .fetch_one(db)
    .await
}

pub async fn update_provider(
    db: &PgPool,
    id: Uuid,
    input: UpdateProvider,
) -> sqlx::Result<Option<ProviderRow>> {
    sqlx::query_as::<_, ProviderRow>(
        r"UPDATE providers SET
               name          = COALESCE($2, name),
               type          = COALESCE($3, type),
               provider_type = COALESCE($4, provider_type),
               base_url      = CASE WHEN $5::text IS NOT NULL THEN $5 ELSE base_url END,
               default_model = CASE WHEN $6::text IS NOT NULL THEN $6 ELSE default_model END,
               enabled       = COALESCE($7, enabled),
               options       = COALESCE($8, options),
               notes         = CASE WHEN $9::text IS NOT NULL THEN $9 ELSE notes END,
               updated_at    = NOW()
           WHERE id = $1
           RETURNING *",
    )
    .bind(id)
    .bind(input.name)
    .bind(input.category)
    .bind(input.provider_type)
    .bind(input.base_url)
    .bind(input.default_model)
    .bind(input.enabled)
    .bind(input.options)
    .bind(input.notes)
    .fetch_optional(db)
    .await
}

pub async fn delete_provider(db: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    let res = sqlx::query("DELETE FROM providers WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(res.rows_affected() > 0)
}

// ── Active ───────────────────────────────────────────────────────────────────

pub async fn list_provider_active(db: &PgPool) -> sqlx::Result<Vec<ProviderActiveRow>> {
    sqlx::query_as::<_, ProviderActiveRow>(
        "SELECT capability, provider_name, priority FROM provider_active ORDER BY capability, priority ASC",
    )
    .fetch_all(db)
    .await
}

/// All active providers for a capability, highest priority first.
pub async fn get_active_providers(db: &PgPool, capability: &str) -> sqlx::Result<Vec<(String, i32)>> {
    sqlx::query_as::<_, (String, i32)>(
        "SELECT provider_name, priority FROM provider_active
         WHERE capability = $1 ORDER BY priority ASC, provider_name",
    )
    .bind(capability)
    .fetch_all(db)
    .await
}

/// Top-priority active provider name for a capability (single-active accessor).
pub async fn get_provider_active(db: &PgPool, capability: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar::<_, String>(
        "SELECT provider_name FROM provider_active
         WHERE capability = $1 ORDER BY priority ASC LIMIT 1",
    )
    .bind(capability)
    .fetch_optional(db)
    .await
}

/// Replace the entire active set for a capability in one transaction.
pub async fn set_provider_active_list(
    db: &PgPool,
    capability: &str,
    entries: &[(String, i32)],
) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM provider_active WHERE capability = $1")
        .bind(capability)
        .execute(&mut *tx)
        .await?;
    for (name, priority) in entries {
        sqlx::query("INSERT INTO provider_active (capability, provider_name, priority) VALUES ($1, $2, $3)")
            .bind(capability)
            .bind(name)
            .bind(priority)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn compaction_capability_name() {
        assert_eq!(CAPABILITY_COMPACTION, "compaction");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn active_priority_roundtrip(pool: sqlx::PgPool) {
        // seed two providers so the FK is satisfied
        for (n, t) in [("ws-a", "websearch"), ("ws-b", "websearch")] {
            sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES ($1,$2,$3,true)")
                .bind(n).bind(t).bind("searxng").execute(&pool).await.unwrap();
        }
        set_provider_active_list(&pool, "websearch", &[("ws-b".into(), 10), ("ws-a".into(), 5)])
            .await.unwrap();

        let ordered = get_active_providers(&pool, "websearch").await.unwrap();
        assert_eq!(ordered, vec![("ws-a".into(), 5), ("ws-b".into(), 10)]);

        // top priority
        assert_eq!(get_provider_active(&pool, "websearch").await.unwrap(), Some("ws-a".into()));

        // replace semantics
        set_provider_active_list(&pool, "websearch", &[("ws-b".into(), 1)]).await.unwrap();
        assert_eq!(get_active_providers(&pool, "websearch").await.unwrap(), vec![("ws-b".into(), 1)]);

        // clear
        set_provider_active_list(&pool, "websearch", &[]).await.unwrap();
        assert!(get_active_providers(&pool, "websearch").await.unwrap().is_empty());
        assert_eq!(get_provider_active(&pool, "websearch").await.unwrap(), None);
    }

    /// Documents the data-loss risk in migration 003_unified_providers.sql.
    ///
    /// The migration merges `llm_providers` and `media_providers` into a unified
    /// `providers` table. The merge uses two INSERT ... ON CONFLICT (name) DO NOTHING
    /// statements executed sequentially:
    ///
    ///   1. INSERT from llm_providers (line 22-25) — uses llm_providers.name as the
    ///      unified name (e.g. "OpenAI", "Anthropic").
    ///   2. INSERT from media_providers (line 28-31) — uses media_providers.id (a slug)
    ///      as the unified name (e.g. "openai-whisper", "toolgate-vision").
    ///
    /// If any media_providers.id matches an already-inserted llm_providers.name, the
    /// media provider row is silently dropped by ON CONFLICT DO NOTHING.
    ///
    /// In production, llm_providers used display names (e.g. "OpenAI") while
    /// media_providers used slugs (e.g. "openai-whisper"), making collision unlikely
    /// but not impossible. If data was lost, re-insert from config.
    #[test]
    fn test_migration_003_conflict_scenario() {
        // Simulate the ON CONFLICT DO NOTHING merge logic in-memory.

        // --- Scenario 1: No collision (typical production case) ---
        let llm_names: Vec<&str> = vec!["OpenAI", "Anthropic", "Ollama"];
        let media_ids: Vec<&str> = vec!["openai-whisper", "toolgate-vision", "toolgate-tts"];

        let mut unified: HashSet<&str> = HashSet::new();

        // Step 1: Insert LLM providers first
        for name in &llm_names {
            unified.insert(name);
        }

        // Step 2: Insert media providers — ON CONFLICT (name) DO NOTHING
        let mut dropped_no_collision = 0;
        for id in &media_ids {
            if !unified.insert(id) {
                dropped_no_collision += 1;
            }
        }

        assert_eq!(
            dropped_no_collision, 0,
            "No rows should be dropped when names don't collide"
        );
        assert_eq!(
            unified.len(),
            llm_names.len() + media_ids.len(),
            "All rows preserved when no name collision"
        );

        // --- Scenario 2: Name collision (demonstrates data loss) ---
        let llm_names_collision: Vec<&str> = vec!["OpenAI", "whisper", "Ollama"];
        let media_ids_collision: Vec<&str> = vec!["whisper", "toolgate-vision", "toolgate-tts"];
        // "whisper" appears in both sets — media provider "whisper" will be silently dropped.

        let mut unified_collision: HashSet<&str> = HashSet::new();

        // Step 1: Insert LLM providers first
        for name in &llm_names_collision {
            unified_collision.insert(name);
        }

        // Step 2: Insert media providers — ON CONFLICT (name) DO NOTHING
        let mut dropped_with_collision = 0;
        for id in &media_ids_collision {
            if !unified_collision.insert(id) {
                dropped_with_collision += 1;
            }
        }

        assert_eq!(
            dropped_with_collision, 1,
            "ON CONFLICT DO NOTHING silently drops the media provider row when names collide"
        );
        assert_eq!(
            unified_collision.len(),
            llm_names_collision.len() + media_ids_collision.len() - 1,
            "One row lost due to name collision — the media provider 'whisper' was silently dropped"
        );
    }
}
