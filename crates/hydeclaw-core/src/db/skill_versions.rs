use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

// ── SkillVersionRow ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct SkillVersionRow {
    pub id: Uuid,
    pub skill_name: String,
    pub generation: i32,
    pub parent_id: Option<Uuid>,
    pub evolution_type: String,
    pub content: String,
    pub content_hash: String,
    pub trigger_reason: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Compute SHA256 hex digest of content.
fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Save a new skill version to the DAG.
///
/// - Computes `content_hash` via SHA256.
/// - Determines `generation` as MAX(generation) + 1 for the given `skill_name` (0 if first).
/// - Inserts the row and returns the new UUID.
pub async fn save_version(
    db: &PgPool,
    skill_name: &str,
    content: &str,
    evolution_type: &str,
    parent_id: Option<Uuid>,
    trigger_reason: Option<&str>,
) -> sqlx::Result<Uuid> {
    let content_hash = sha256_hex(content);

    let row = sqlx::query(
        "INSERT INTO skill_versions \
         (skill_name, generation, parent_id, evolution_type, content, content_hash, trigger_reason) \
         VALUES ($1, \
             (SELECT COALESCE(MAX(generation), -1) + 1 FROM skill_versions WHERE skill_name = $1), \
             $2, $3, $4, $5, $6) \
         RETURNING id",
    )
    .bind(skill_name)
    .bind(parent_id)
    .bind(evolution_type)
    .bind(content)
    .bind(&content_hash)
    .bind(trigger_reason)
    .fetch_one(db)
    .await?;

    Ok(row.get("id"))
}

// ── Query helpers ─────────────────────────────────────────────────────────────

/// Return all versions for a skill ordered newest-first (by generation).
pub async fn list_versions(db: &PgPool, skill_name: &str) -> sqlx::Result<Vec<SkillVersionRow>> {
    sqlx::query_as::<_, SkillVersionRow>(
        "SELECT * FROM skill_versions WHERE skill_name = $1 ORDER BY generation DESC",
    )
    .bind(skill_name)
    .fetch_all(db)
    .await
}

/// Return a single version by UUID, or None if not found.
pub async fn get_version(db: &PgPool, id: Uuid) -> sqlx::Result<Option<SkillVersionRow>> {
    sqlx::query_as::<_, SkillVersionRow>(
        "SELECT * FROM skill_versions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}
