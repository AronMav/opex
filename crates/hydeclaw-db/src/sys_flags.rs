//! Typed helpers for the `system_flags` JSONB key/value table.
//!
//! Replaces inline SQL scattered across handlers. Always use these helpers
//! for `system_flags` access — direct SQL fragments are technical debt.

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

/// Return the JSON value stored under `key`, or `None` if no row exists.
///
/// Lenient: swallows SQL errors and returns `None`. Use for best-effort
/// reads (feature flag checks, UI hints). For migration gates and other
/// operations that must distinguish "missing" from "DB unavailable", use
/// [`try_get`] instead.
pub async fn get(db: &PgPool, key: &str) -> Option<Value> {
    sqlx::query_scalar::<_, Value>("SELECT value FROM system_flags WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

/// Strict variant of [`get`]: returns `Err` on SQL failure.
///
/// Use this for migration gates and other operations where you need to
/// distinguish "key missing" (`Ok(None)`) from "DB unavailable" (`Err`).
pub async fn try_get(db: &PgPool, key: &str) -> Result<Option<Value>> {
    sqlx::query_scalar::<_, Value>("SELECT value FROM system_flags WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .with_context(|| format!("failed to read system_flags key {key}"))
}

/// Insert or update `key` with `value` (UPSERT via ON CONFLICT).
///
/// Also refreshes `updated_at = NOW()` on conflict — preserves the
/// diagnostic signal the old inline SQL provided before this helper.
pub async fn upsert(db: &PgPool, key: &str, value: Value) -> Result<()> {
    sqlx::query(
        "INSERT INTO system_flags (key, value) VALUES ($1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await
    .with_context(|| format!("failed to upsert system_flags key {key}"))?;
    Ok(())
}

/// Delete the row for `key`. No-op if it does not exist.
pub async fn delete(db: &PgPool, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM system_flags WHERE key = $1")
        .bind(key)
        .execute(db)
        .await
        .with_context(|| format!("failed to delete system_flags key {key}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[sqlx::test]
    async fn get_returns_none_for_missing_key(pool: PgPool) {
        assert!(get(&pool, "no.such.key").await.is_none());
    }

    #[sqlx::test]
    async fn upsert_then_get_roundtrip(pool: PgPool) {
        upsert(&pool, "test.key", json!(42)).await.unwrap();
        assert_eq!(get(&pool, "test.key").await, Some(json!(42)));
    }

    #[sqlx::test]
    async fn upsert_overwrites_existing(pool: PgPool) {
        upsert(&pool, "test.key", json!("first")).await.unwrap();
        upsert(&pool, "test.key", json!("second")).await.unwrap();
        assert_eq!(get(&pool, "test.key").await, Some(json!("second")));
    }

    #[sqlx::test]
    async fn delete_removes_row(pool: PgPool) {
        upsert(&pool, "test.key", json!(1)).await.unwrap();
        delete(&pool, "test.key").await.unwrap();
        assert_eq!(get(&pool, "test.key").await, None);
    }

    #[sqlx::test]
    async fn delete_missing_is_noop(pool: PgPool) {
        // не должен падать
        delete(&pool, "no.such.key").await.unwrap();
    }
}
