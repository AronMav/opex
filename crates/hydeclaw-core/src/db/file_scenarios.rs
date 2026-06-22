//! `file_scenarios` table CRUD. See docs/superpowers/specs/2026-06-22-file-scenario-engine-design.md §4.1.

// All public items in this module are forward-interface consumed by Phase 5
// (bindings CRUD API / HTTP routes). Remove this allow once Phase 5 lands.
#![allow(dead_code)]

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct FileScenarioRow {
    pub id: Uuid,
    pub match_type: String,
    pub executor: String,
    pub action_ref: String,
    pub label: String,
    pub is_default: bool,
    pub priority: i32,
    pub enabled: bool,
    pub scope: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const SELECT_COLS: &str =
    "id, match_type, executor, action_ref, label, is_default, priority, enabled, scope, created_by, created_at, updated_at";

/// List all bindings, default-first then by priority (lowest wins) then created_at.
pub async fn list(pool: &PgPool) -> Result<Vec<FileScenarioRow>> {
    let sql = format!(
        "SELECT {SELECT_COLS} FROM file_scenarios \
         ORDER BY match_type, is_default DESC, priority ASC, created_at ASC, id ASC"
    );
    Ok(sqlx::query_as::<_, FileScenarioRow>(&sql).fetch_all(pool).await?)
}

/// Return enabled bindings whose `match_type` glob matches `sniffed_mime`, ordered
/// by `is_default DESC, priority ASC` (highest-priority default first).
///
/// Glob semantics (FSE §4.2):
/// - `*`        — matches any mime type
/// - `image/*`  — matches any mime whose family (before `/`) equals `image`
/// - `audio/*`  — same for audio, video, application, etc.
/// - anything else is an exact string match (e.g. `application/pdf`)
///
/// Only `enabled = true` bindings are returned. Matching is done in Rust after a
/// full table scan; the table is operator-configured and stays tiny (< 100 rows).
pub async fn list_enabled_for_match_type(
    pool: &PgPool,
    sniffed_mime: &str,
) -> Result<Vec<FileScenarioRow>> {
    let sql = format!(
        "SELECT {SELECT_COLS} FROM file_scenarios WHERE enabled = true \
         ORDER BY is_default DESC, priority ASC, created_at ASC, id ASC"
    );
    let all: Vec<FileScenarioRow> = sqlx::query_as::<_, FileScenarioRow>(&sql).fetch_all(pool).await?;

    let mime_family = sniffed_mime.split('/').next().unwrap_or("");
    let matched = all
        .into_iter()
        .filter(|row| mime_glob_matches(&row.match_type, sniffed_mime, mime_family))
        .collect();
    Ok(matched)
}

/// Test whether a `match_type` pattern matches `sniffed_mime`.
/// `mime_family` is the part before `/` in `sniffed_mime` (pre-computed by caller).
fn mime_glob_matches(pattern: &str, sniffed_mime: &str, mime_family: &str) -> bool {
    if pattern == "*" || pattern == "*/*" {
        return true;
    }
    if let Some(family) = pattern.strip_suffix("/*") {
        return family == mime_family;
    }
    pattern == sniffed_mime
}

pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<Option<FileScenarioRow>> {
    let sql = format!("SELECT {SELECT_COLS} FROM file_scenarios WHERE id = $1");
    Ok(sqlx::query_as::<_, FileScenarioRow>(&sql).bind(id).fetch_optional(pool).await?)
}

/// Insert a binding. Caller is responsible for FSE_DEFAULT_ALLOWLIST / executor
/// validation (enforced at the HTTP/tool layer in a later phase).
#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    match_type: &str,
    executor: &str,
    action_ref: &str,
    label: &str,
    is_default: bool,
    priority: i32,
    enabled: bool,
    created_by: &str,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO file_scenarios \
         (id, match_type, executor, action_ref, label, is_default, priority, enabled, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(match_type)
    .bind(executor)
    .bind(action_ref)
    .bind(label)
    .bind(is_default)
    .bind(priority)
    .bind(enabled)
    .bind(created_by)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Update mutable fields. Returns rows affected (0 or 1).
pub async fn update(pool: &PgPool, id: Uuid, label: &str, priority: i32, enabled: bool) -> Result<u64> {
    let res = sqlx::query(
        "UPDATE file_scenarios SET label = $2, priority = $3, enabled = $4, updated_at = NOW() WHERE id = $1",
    )
    .bind(id)
    .bind(label)
    .bind(priority)
    .bind(enabled)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub async fn delete(pool: &PgPool, id: Uuid) -> Result<u64> {
    let res = sqlx::query("DELETE FROM file_scenarios WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// Promote `id` to the default for its match_type, clearing the prior default in
/// one transaction so the `file_scenarios_one_default` partial unique index is
/// never transiently violated.
pub async fn set_default(pool: &PgPool, id: Uuid) -> Result<()> {
    let mut tx = pool.begin().await?;
    let match_type: String = sqlx::query_scalar("SELECT match_type FROM file_scenarios WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("UPDATE file_scenarios SET is_default = false, updated_at = NOW() WHERE match_type = $1 AND is_default")
        .bind(&match_type)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE file_scenarios SET is_default = true, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Record a per-file processing outcome. Returns the new row id.
#[allow(clippy::too_many_arguments)]
pub async fn insert_outcome(
    pool: &PgPool,
    session_id: Uuid,
    upload_id: Uuid,
    match_type: &str,
    scenario_id: Option<Uuid>,
    status: &str,
    reason: Option<&str>,
    duration_ms: i64,
    bytes: i64,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO file_scenario_outcomes \
         (id, session_id, upload_id, match_type, scenario_id, status, reason, duration_ms, bytes) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(session_id)
    .bind(upload_id)
    .bind(match_type)
    .bind(scenario_id)
    .bind(status)
    .bind(reason)
    .bind(duration_ms)
    .bind(bytes)
    .execute(pool)
    .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{create, get_by_id, insert_outcome, list, set_default};

    // ── Pure unit tests for mime_glob_matches (no DB) ────────────────────────

    #[test]
    fn mime_glob_universal_wildcard() {
        use super::mime_glob_matches;
        // "*" and "*/*" must both match any mime
        assert!(mime_glob_matches("*", "image/png", "image"), "* matches image/png");
        assert!(mime_glob_matches("*/*", "image/png", "image"), "*/* matches image/png");
        assert!(mime_glob_matches("*/*", "audio/mpeg", "audio"), "*/* matches audio/mpeg");
        assert!(mime_glob_matches("*/*", "application/pdf", "application"), "*/* matches application/pdf");
    }

    #[test]
    fn mime_glob_family_wildcard() {
        use super::mime_glob_matches;
        assert!(mime_glob_matches("image/*", "image/png", "image"), "image/* matches image/png");
        assert!(mime_glob_matches("image/*", "image/jpeg", "image"), "image/* matches image/jpeg");
        assert!(!mime_glob_matches("image/*", "audio/mpeg", "audio"), "image/* does NOT match audio/mpeg");
    }

    #[test]
    fn mime_glob_exact_match() {
        use super::mime_glob_matches;
        assert!(mime_glob_matches("application/pdf", "application/pdf", "application"), "exact match works");
        assert!(!mime_glob_matches("application/pdf", "image/png", "image"), "exact match rejects wrong mime");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn create_then_get_round_trip(pool: PgPool) {
        let id = create(&pool, "image/*", "tool", "describe", "Describe", true, 50, true, "system")
            .await
            .unwrap();
        let row = get_by_id(&pool, id).await.unwrap().expect("row present");
        assert_eq!(row.match_type, "image/*");
        assert_eq!(row.executor, "tool");
        assert_eq!(row.action_ref, "describe");
        assert!(row.is_default);
        assert_eq!(row.priority, 50);
        assert_eq!(list(&pool).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn set_default_flips_within_match_type(pool: PgPool) {
        let a = create(&pool, "audio/*", "tool", "transcribe", "Transcribe", true, 100, true, "system").await.unwrap();
        let b = create(&pool, "audio/*", "skill", "fancy", "Fancy", false, 100, true, "ui").await.unwrap();

        // Promote b to default — a must be demoted in the same transaction (else the
        // partial unique index would be violated).
        set_default(&pool, b).await.unwrap();

        assert!(!get_by_id(&pool, a).await.unwrap().unwrap().is_default, "old default cleared");
        assert!(get_by_id(&pool, b).await.unwrap().unwrap().is_default, "new default set");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_outcome_round_trip(pool: PgPool) {
        let oid = insert_outcome(
            &pool, Uuid::new_v4(), Uuid::new_v4(), "application/pdf",
            None, "failed", Some("toolgate 502"), 880, 2048,
        )
        .await
        .unwrap();
        let (status, reason): (String, Option<String>) = sqlx::query_as(
            r#"SELECT status, reason FROM file_scenario_outcomes WHERE id = $1"#,
        )
        .bind(oid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(reason.as_deref(), Some("toolgate 502"));
    }

    // ── Tests from Tasks 1.1–1.3 (table/index/constraint verification) ──────

    #[sqlx::test(migrations = "../../migrations")]
    async fn one_default_per_match_type_enforced(pool: PgPool) {
        // First default for image/* succeeds.
        sqlx::query(
            r#"INSERT INTO file_scenarios
               (id, match_type, executor, action_ref, label, is_default, created_by)
               VALUES ($1, 'image/*', 'tool', 'describe', 'Describe', true, 'system')"#,
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .expect("first default inserts");

        // Second default for the SAME match_type violates the partial unique index.
        let err = sqlx::query(
            r#"INSERT INTO file_scenarios
               (id, match_type, executor, action_ref, label, is_default, created_by)
               VALUES ($1, 'image/*', 'skill', 'fancy-describe', 'Fancy', true, 'ui')"#,
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(err.is_err(), "second default for image/* must violate file_scenarios_one_default");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn unique_match_type_action_ref(pool: PgPool) {
        sqlx::query(
            r#"INSERT INTO file_scenarios
               (id, match_type, executor, action_ref, label, created_by)
               VALUES ($1, 'audio/*', 'tool', 'transcribe', 'Transcribe', 'system')"#,
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .expect("first row inserts");

        let dup = sqlx::query(
            r#"INSERT INTO file_scenarios
               (id, match_type, executor, action_ref, label, created_by)
               VALUES ($1, 'audio/*', 'tool', 'transcribe', 'Dup', 'ui')"#,
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(dup.is_err(), "duplicate (match_type, action_ref) must be rejected");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn executor_check_rejects_bad_value(pool: PgPool) {
        let bad = sqlx::query(
            r#"INSERT INTO file_scenarios
               (id, match_type, executor, action_ref, label, created_by)
               VALUES ($1, 'image/*', 'wormhole', 'x', 'X', 'ui')"#,
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(bad.is_err(), "executor must be CHECK-constrained to tool|skill");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn uploads_cap_relaxed_to_50mb(pool: PgPool) {
        // A row at 30 MB must be accepted after the relax migration (was rejected by 052's 20 MB CHECK).
        let id = Uuid::new_v4();
        let res = sqlx::query(
            r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
               VALUES ($1, 'tool_output', NULL, 'application/pdf', '\x00', '\x00', 31457280, NULL)"#,
        )
        .bind(id)
        .execute(&pool)
        .await;
        assert!(res.is_ok(), "30 MB size_bytes must pass after migration 062 relaxes the CHECK");

        // 60 MB still rejected by the new 50 MB CHECK.
        let over = sqlx::query(
            r#"INSERT INTO uploads (id, owner_type, owner_id, mime, data, sha256, size_bytes, expires_at)
               VALUES ($1, 'tool_output', NULL, 'application/pdf', '\x00', '\x00', 62914560, NULL)"#,
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(over.is_err(), "60 MB must still violate the new 50 MB CHECK");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn outcomes_row_round_trip(pool: PgPool) {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO file_scenario_outcomes
               (id, session_id, upload_id, match_type, scenario_id, status, reason, duration_ms, bytes)
               VALUES ($1, $2, $3, 'application/pdf', NULL, 'ok', NULL, 1234, 4096)"#,
        )
        .bind(id)
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .expect("outcome inserts with NULL scenario_id and NULL reason");

        let (status, dur, bytes): (String, i64, i64) = sqlx::query_as(
            r#"SELECT status, duration_ms, bytes FROM file_scenario_outcomes WHERE id = $1"#,
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "ok");
        assert_eq!(dur, 1234);
        assert_eq!(bytes, 4096);
    }
}
