use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

pub const DEFAULT_PROFILE: &str = "Default";

/// Capability-ключи слотов. `text` принимает провайдеров категории text|llm,
/// остальные — категорию с тем же именем.
pub const PROFILE_CAPABILITIES: [&str; 7] =
    ["text", "compaction", "stt", "tts", "vision", "imagegen", "websearch"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotEntry {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

pub type Slots = HashMap<String, Vec<SlotEntry>>;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProfileRow {
    pub id: Uuid,
    pub name: String,
    pub slots: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProfileRow {
    /// Толерантный парс: битый JSONB → пустые слоты (лог на совести вызывающего).
    pub fn parsed_slots(&self) -> Slots {
        serde_json::from_value(self.slots.clone()).unwrap_or_default()
    }
}

pub async fn list_profiles(db: &PgPool) -> sqlx::Result<Vec<ProfileRow>> {
    sqlx::query_as("SELECT * FROM profiles ORDER BY name").fetch_all(db).await
}

pub async fn get_profile(db: &PgPool, id: Uuid) -> sqlx::Result<Option<ProfileRow>> {
    sqlx::query_as("SELECT * FROM profiles WHERE id = $1").bind(id).fetch_optional(db).await
}

pub async fn get_profile_by_name(db: &PgPool, name: &str) -> sqlx::Result<Option<ProfileRow>> {
    sqlx::query_as("SELECT * FROM profiles WHERE name = $1").bind(name).fetch_optional(db).await
}

pub async fn create_profile(db: &PgPool, name: &str, slots: &Slots) -> sqlx::Result<ProfileRow> {
    sqlx::query_as(
        "INSERT INTO profiles (name, slots) VALUES ($1, $2) RETURNING *")
        .bind(name)
        .bind(serde_json::to_value(slots).unwrap_or(serde_json::json!({})))
        .fetch_one(db).await
}

/// name/slots опциональны — None означает «не менять».
pub async fn update_profile(
    db: &PgPool, id: Uuid, name: Option<&str>, slots: Option<&Slots>,
) -> sqlx::Result<Option<ProfileRow>> {
    sqlx::query_as(
        "UPDATE profiles SET \
           name = COALESCE($2, name), \
           slots = COALESCE($3, slots), \
           updated_at = now() \
         WHERE id = $1 RETURNING *")
        .bind(id)
        .bind(name)
        .bind(slots.map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({}))))
        .fetch_optional(db).await
}

pub async fn delete_profile(db: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    let res = sqlx::query("DELETE FROM profiles WHERE id = $1").bind(id).execute(db).await?;
    Ok(res.rows_affected() > 0)
}

/// Копия с уникализированным именем: "{name} (copy)", "{name} (copy 2)", …
pub async fn copy_profile(db: &PgPool, id: Uuid) -> sqlx::Result<Option<ProfileRow>> {
    let Some(src) = get_profile(db, id).await? else { return Ok(None) };
    let mut n = 1usize;
    loop {
        let candidate = if n == 1 { format!("{} (copy)", src.name) } else { format!("{} (copy {n})", src.name) };
        if get_profile_by_name(db, &candidate).await?.is_none() {
            let row = sqlx::query_as(
                "INSERT INTO profiles (name, slots) VALUES ($1, $2) RETURNING *")
                .bind(&candidate).bind(&src.slots).fetch_one(db).await?;
            return Ok(Some(row));
        }
        n += 1;
    }
}

/// Валидация слотов: известные capability, непустые имена, существование
/// записи providers подходящей категории. `text`/`compaction` принимают
/// категории text|llm; остальные — одноимённую категорию.
pub async fn validate_slots(db: &PgPool, slots: &Slots) -> Result<(), String> {
    for (cap, entries) in slots {
        if !PROFILE_CAPABILITIES.contains(&cap.as_str()) {
            return Err(format!("unknown capability '{cap}'"));
        }
        for e in entries {
            if e.provider.trim().is_empty() {
                return Err(format!("empty provider name in '{cap}' slot"));
            }
            let row = super::providers::get_provider_by_name(db, &e.provider)
                .await.map_err(|e| e.to_string())?
                .ok_or_else(|| format!("provider '{}' not found", e.provider))?;
            let ok = match cap.as_str() {
                "text" | "compaction" => row.category == "text" || row.category == "llm",
                other => row.category == other,
            };
            if !ok {
                return Err(format!(
                    "provider '{}' has category '{}', slot '{cap}' expects a matching category",
                    e.provider, row.category));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(provider: &str) -> SlotEntry {
        SlotEntry { provider: provider.into(), model: None, voice: None }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn crud_roundtrip(pool: sqlx::PgPool) {
        let mut slots = Slots::new();
        slots.insert("tts".into(), vec![SlotEntry { provider: "mm".into(), model: None, voice: Some("Champ".into()) }]);
        let created = create_profile(&pool, "P1", &slots).await.unwrap();
        assert_eq!(created.name, "P1");
        let fetched = get_profile_by_name(&pool, "P1").await.unwrap().unwrap();
        assert_eq!(fetched.parsed_slots()["tts"][0].voice.as_deref(), Some("Champ"));

        slots.insert("stt".into(), vec![slot("whisper")]);
        let updated = update_profile(&pool, created.id, Some("P1x"), Some(&slots)).await.unwrap().unwrap();
        assert_eq!(updated.name, "P1x");
        assert_eq!(updated.parsed_slots().len(), 2);

        assert!(delete_profile(&pool, created.id).await.unwrap());
        assert!(get_profile(&pool, created.id).await.unwrap().is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn copy_uniquifies_name(pool: sqlx::PgPool) {
        let p = create_profile(&pool, "Base", &Slots::new()).await.unwrap();
        let c1 = copy_profile(&pool, p.id).await.unwrap().unwrap();
        assert_eq!(c1.name, "Base (copy)");
        let c2 = copy_profile(&pool, p.id).await.unwrap().unwrap();
        assert_eq!(c2.name, "Base (copy 2)");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn validate_rejects_bad_slots(pool: sqlx::PgPool) {
        // unknown capability
        let mut bad = Slots::new();
        bad.insert("smellgen".into(), vec![slot("x")]);
        assert!(validate_slots(&pool, &bad).await.is_err());
        // empty provider name
        let mut bad2 = Slots::new();
        bad2.insert("tts".into(), vec![slot("")]);
        assert!(validate_slots(&pool, &bad2).await.is_err());
        // provider not in table
        let mut bad3 = Slots::new();
        bad3.insert("tts".into(), vec![slot("ghost")]);
        assert!(validate_slots(&pool, &bad3).await.is_err());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn validate_checks_category(pool: sqlx::PgPool) {
        sqlx::query("INSERT INTO providers (name, type, provider_type) VALUES ('tts-p','tts','minimax'),('llm-p','llm','openai_compat')")
            .execute(&pool).await.unwrap();
        let mut ok = Slots::new();
        ok.insert("tts".into(), vec![slot("tts-p")]);
        ok.insert("text".into(), vec![slot("llm-p")]);   // text принимает категории text|llm
        ok.insert("compaction".into(), vec![slot("llm-p")]);
        assert!(validate_slots(&pool, &ok).await.is_ok());

        let mut cross = Slots::new();
        cross.insert("tts".into(), vec![slot("llm-p")]); // категория-mismatch
        assert!(validate_slots(&pool, &cross).await.is_err());
    }
}
