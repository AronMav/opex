//! `uploads` table CRUD. See docs/superpowers/specs/2026-05-15-uploads-to-db-design.md.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

/// Hard upper bound on a single uploads.data row, matching the DB CHECK in
/// `migrations/062_uploads_relax_cap.sql` (relaxed from 052's 20 MB). This is
/// the DB-backstop; the runtime request-layer ceiling is `[uploads]
/// max_upload_bytes`. Callers must reject larger inputs before INSERT — the DB
/// will refuse a CHECK violation otherwise.
pub const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
#[allow(dead_code)]   // id/expires_at/filename part of the query shape; reserved for diagnostics
pub struct UploadRow {
    pub id: Uuid,
    pub mime: String,
    pub data: Vec<u8>,
    pub sha256: Vec<u8>,
    pub size_bytes: i64,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Original client-side filename when known (user uploads). Absent for
    /// tool_output binaries and agent icons. Surfaced in Content-Disposition
    /// by `uploads_serve` so downloads keep their original name.
    pub filename: Option<String>,
}

/// Insert or replace the icon for an agent. Returns the new row id.
pub async fn upsert_agent_icon(
    pool: &PgPool,
    agent_name: &str,
    mime: &str,
    data: &[u8],
) -> Result<Uuid> {
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(anyhow!("upload exceeds {} byte cap", MAX_UPLOAD_BYTES));
    }
    let new_id = Uuid::new_v4();
    let sha = Sha256::digest(data).to_vec();
    let size = i64::try_from(data.len()).map_err(|_| anyhow!("data too large"))?;

    let row = sqlx::query(
        r#"
        INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
        VALUES ($1, 'agent_icon', $2, $3, $4, $5, $6, NULL)
        ON CONFLICT (owner_id) WHERE owner_type = 'agent_icon'
        DO UPDATE SET
            id = EXCLUDED.id,
            mime = EXCLUDED.mime,
            data = EXCLUDED.data,
            sha256 = EXCLUDED.sha256,
            size_bytes = EXCLUDED.size_bytes,
            created_at = NOW()
        RETURNING id
        "#,
    )
    .bind(new_id)
    .bind(agent_name)
    .bind(mime)
    .bind(data)
    .bind(&sha)
    .bind(size)
    .fetch_one(pool)
    .await?;

    Ok(row.try_get::<Uuid, _>("id")?)
}

/// Insert a tool_output or client_upload row with retention TTL. Returns the row id.
///
/// `filename` is the original client-side filename (user uploads) — surfaced in
/// the serve endpoint's Content-Disposition so downloads keep their real name
/// instead of the row UUID. Pass `None` for tool outputs / icons / anything
/// that lacks a meaningful name.
pub async fn insert_with_retention(
    pool: &PgPool,
    owner_type: &str,
    owner_id: Option<&str>,
    mime: &str,
    data: &[u8],
    retention_days: u32,
    filename: Option<&str>,
) -> Result<Uuid> {
    if owner_type != "tool_output" && owner_type != "client_upload" {
        return Err(anyhow!("owner_type must be tool_output or client_upload"));
    }
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(anyhow!("upload exceeds {} byte cap", MAX_UPLOAD_BYTES));
    }
    let id = Uuid::new_v4();
    let sha = Sha256::digest(data).to_vec();
    let size = i64::try_from(data.len()).map_err(|_| anyhow!("data too large"))?;

    sqlx::query(
        r#"
        INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at, filename)
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW() + ($8::INT * INTERVAL '1 day'), $9)
        "#,
    )
    .bind(id)
    .bind(owner_type)
    .bind(owner_id)
    .bind(mime)
    .bind(data)
    .bind(&sha)
    .bind(size)
    .bind(i32::try_from(retention_days).unwrap_or(30))
    .bind(filename)
    .execute(pool)
    .await?;

    Ok(id)
}

/// Read a row by id. Returns None if missing OR expired.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<Option<UploadRow>> {
    let row = sqlx::query(
        r#"
        SELECT id, mime, data, sha256, size_bytes, expires_at, filename
        FROM uploads
        WHERE id = $1 AND (expires_at IS NULL OR expires_at > NOW())
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| UploadRow {
        id: r.try_get("id").unwrap(),
        mime: r.try_get("mime").unwrap(),
        data: r.try_get("data").unwrap(),
        sha256: r.try_get("sha256").unwrap(),
        size_bytes: r.try_get("size_bytes").unwrap(),
        expires_at: r.try_get("expires_at").ok().flatten(),
        filename: r.try_get("filename").ok().flatten(),
    }))
}

/// Read just the id of an agent's icon (cheap — no BYTEA fetch). Used by tests
/// and as a single-agent fast path; handlers prefer the batch
/// `list_agent_icon_ids` to avoid N+1 lookups.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn lookup_agent_icon_id(pool: &PgPool, agent_name: &str) -> Result<Option<Uuid>> {
    let row = sqlx::query(
        r#"SELECT id FROM uploads WHERE owner_type = 'agent_icon' AND owner_id = $1"#,
    )
    .bind(agent_name)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.try_get::<Uuid, _>("id").ok()))
}

/// Batch lookup for DTO factories. Returns map: agent_name -> upload id.
pub async fn list_agent_icon_ids(
    pool: &PgPool,
    agent_names: &[String],
) -> Result<HashMap<String, Uuid>> {
    if agent_names.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        r#"
        SELECT owner_id, id FROM uploads
        WHERE owner_type = 'agent_icon' AND owner_id = ANY($1)
        "#,
    )
    .bind(agent_names)
    .fetch_all(pool)
    .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for r in rows {
        let owner_id: String = r.try_get("owner_id")?;
        let id: Uuid = r.try_get("id")?;
        map.insert(owner_id, id);
    }
    Ok(map)
}

/// Delete the icon for an agent. No-op if absent. Returns rows affected (0 or 1).
#[allow(dead_code)] // sole caller was the removed DELETE /api/agents/{name}/icon.
pub async fn delete_agent_icon(pool: &PgPool, agent_name: &str) -> Result<u64> {
    let result = sqlx::query(
        r#"DELETE FROM uploads WHERE owner_type = 'agent_icon' AND owner_id = $1"#,
    )
    .bind(agent_name)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Cleanup expired rows. Returns count deleted.
pub async fn cleanup_expired(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"DELETE FROM uploads WHERE expires_at IS NOT NULL AND expires_at < NOW()"#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn agent_icon_upsert_inserts_then_replaces(pool: PgPool) {
        let id1 = upsert_agent_icon(&pool, "Opex", "image/png", b"first").await.unwrap();
        let id2 = upsert_agent_icon(&pool, "Opex", "image/jpeg", b"second-and-larger").await.unwrap();
        assert_ne!(id1, id2, "upsert must produce new id on replace");

        // Only one row remains for the agent.
        let count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM uploads WHERE owner_type = 'agent_icon' AND owner_id = 'Opex'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        // Latest data wins.
        let row = get_by_id(&pool, id2).await.unwrap().unwrap();
        assert_eq!(row.mime, "image/jpeg");
        assert_eq!(row.data, b"second-and-larger");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn lookup_agent_icon_id_returns_current(pool: PgPool) {
        assert!(lookup_agent_icon_id(&pool, "Opex").await.unwrap().is_none());
        let id = upsert_agent_icon(&pool, "Opex", "image/png", b"x").await.unwrap();
        assert_eq!(lookup_agent_icon_id(&pool, "Opex").await.unwrap(), Some(id));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_agent_icon_ids_batch(pool: PgPool) {
        upsert_agent_icon(&pool, "Opex", "image/png", b"h").await.unwrap();
        upsert_agent_icon(&pool, "Alma", "image/png", b"a").await.unwrap();
        let names = vec!["Opex".to_string(), "Alma".to_string(), "Missing".to_string()];
        let map = list_agent_icon_ids(&pool, &names).await.unwrap();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("Opex"));
        assert!(map.contains_key("Alma"));
        assert!(!map.contains_key("Missing"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_agent_icon_ids_empty_input(pool: PgPool) {
        let map = list_agent_icon_ids(&pool, &[]).await.unwrap();
        assert!(map.is_empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn delete_agent_icon_returns_count(pool: PgPool) {
        assert_eq!(delete_agent_icon(&pool, "Opex").await.unwrap(), 0);
        upsert_agent_icon(&pool, "Opex", "image/png", b"x").await.unwrap();
        assert_eq!(delete_agent_icon(&pool, "Opex").await.unwrap(), 1);
        assert!(lookup_agent_icon_id(&pool, "Opex").await.unwrap().is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_with_retention_sets_expires_at(pool: PgPool) {
        let id = insert_with_retention(&pool, "tool_output", Some("msg-uuid"), "audio/mp3", b"audio-bytes", 30, None).await.unwrap();
        let row = get_by_id(&pool, id).await.unwrap().unwrap();
        assert!(row.expires_at.is_some());

        let exp = row.expires_at.unwrap();
        let now = chrono::Utc::now();
        let delta = (exp - now).num_days();
        assert!((29..=31).contains(&delta), "expires ~30 days from now, got {delta} day delta");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_with_retention_rejects_unknown_owner_type(pool: PgPool) {
        let result = insert_with_retention(&pool, "bogus", None, "image/png", b"x", 30, None).await;
        assert!(result.is_err());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_with_retention_persists_filename(pool: PgPool) {
        // The client-side filename survives the round-trip so the serve
        // endpoint can ship it in Content-Disposition (fix for downloads
        // landing as their UUID instead of the original name).
        let id = insert_with_retention(
            &pool,
            "client_upload",
            None,
            "application/json",
            b"{}",
            30,
            Some("chroma_api.json"),
        )
        .await
        .unwrap();
        let row = get_by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.filename.as_deref(), Some("chroma_api.json"));

        // None stays None (tool_output / icon paths).
        let id2 = insert_with_retention(&pool, "tool_output", None, "image/png", b"x", 30, None)
            .await
            .unwrap();
        let row2 = get_by_id(&pool, id2).await.unwrap().unwrap();
        assert!(row2.filename.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn get_by_id_hides_expired(pool: PgPool) {
        // Insert a row that already expired (retention = -1 days).
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
               VALUES ($1, 'tool_output', NULL, 'image/png', $2, $3, $4, NOW() - INTERVAL '1 day')"#,
        )
        .bind(id).bind(b"x" as &[u8]).bind(vec![0u8; 32]).bind(1_i64)
        .execute(&pool).await.unwrap();

        assert!(get_by_id(&pool, id).await.unwrap().is_none(), "expired row must not surface");
    }

    #[test]
    fn max_upload_bytes_is_50mb() {
        // Must match migration 062's relaxed CHECK (52_428_800) so the const
        // backstop never rejects a row the DB would accept.
        assert_eq!(super::MAX_UPLOAD_BYTES, 52_428_800);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_expired_deletes_only_expired(pool: PgPool) {
        // One expired tool_output, one fresh tool_output, one permanent agent_icon.
        sqlx::query(
            r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
               VALUES (gen_random_uuid(), 'tool_output', NULL, 'a', '\x00', '\x00', 1, NOW() - INTERVAL '1 day'),
                      (gen_random_uuid(), 'tool_output', NULL, 'a', '\x00', '\x00', 1, NOW() + INTERVAL '1 day'),
                      (gen_random_uuid(), 'agent_icon', 'Opex', 'a', '\x00', '\x00', 1, NULL)"#,
        )
        .execute(&pool).await.unwrap();

        let deleted = cleanup_expired(&pool).await.unwrap();
        assert_eq!(deleted, 1, "exactly one expired row deleted");

        let remaining: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM uploads"#).fetch_one(&pool).await.unwrap();
        assert_eq!(remaining, 2);
    }
}
