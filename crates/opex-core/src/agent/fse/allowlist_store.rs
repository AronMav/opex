//! Operator-editable closed-domain allowlist toggle, persisted in the
//! `system_flags` JSONB table under key `fse.allowlist.enabled` as a JSON
//! string array. Unset == "all constant members enabled" (the seeded
//! default). Writes are validated against `FSE_DEFAULT_ALLOWLIST` so the
//! toggle can never admit a non-built-in action (design §4.6).

use sqlx::PgPool;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::allowlist::{validate_allowlist_toggle, AllowlistError, FSE_DEFAULT_ALLOWLIST};

/// `system_flags` key holding the JSON array of currently-enabled members.
const ALLOWLIST_FLAG_KEY: &str = "fse.allowlist.enabled";

/// TTL for the in-process allowlist cache. The allowlist is a global
/// operator setting that changes rarely — 30s is a safe balance between
/// freshness and DB query avoidance.
const ALLOWLIST_CACHE_TTL: Duration = Duration::from_secs(30);

/// Process-wide cache for `get_enabled_allowlist`. Avoids a DB query on
/// every `file_handler` call (which can fire twice per user interaction:
/// list + run). The cache is invalidated on writes via
/// `invalidate_allowlist_cache`.
static ALLOWLIST_CACHE: Mutex<Option<(Instant, Arc<Vec<String>>)>> = Mutex::new(None);

/// Return the enabled subset of `FSE_DEFAULT_ALLOWLIST`. When the flag is
/// unset (fresh install) or unreadable, defaults to the FULL constant — the
/// seeded defaults must work out of the box. Any stale value not in the
/// constant is silently dropped (defense-in-depth against a hand-edited row).
///
/// Results are cached for `ALLOWLIST_CACHE_TTL` to avoid a DB query on
/// every `file_handler` invocation.
pub async fn get_enabled_allowlist(db: &PgPool) -> Vec<String> {
    // Check cache first (fast path — no DB query)
    {
        let cache = ALLOWLIST_CACHE.lock().unwrap();
        if let Some((fetched_at, cached)) = cache.as_ref()
            && fetched_at.elapsed() < ALLOWLIST_CACHE_TTL
        {
            return (*cached).to_vec();
        }
    }

    // Cache miss or expired — fetch from DB
    let stored: Option<Vec<String>> = opex_db::sys_flags::get(db, ALLOWLIST_FLAG_KEY)
        .await
        .and_then(|v| serde_json::from_value(v).ok());
    let result: Vec<String> = match stored {
        Some(list) => list
            .into_iter()
            .filter(|m| FSE_DEFAULT_ALLOWLIST.contains(&m.as_str()))
            .collect(),
        None => FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect(),
    };

    // Update cache
    {
        let mut cache = ALLOWLIST_CACHE.lock().unwrap();
        *cache = Some((Instant::now(), Arc::new(result.clone())));
    }

    result
}

/// Invalidate the allowlist cache. Called after writes (set_enabled_allowlist*)
/// so the next read picks up the change immediately.
pub fn invalidate_allowlist_cache() {
    let mut cache = ALLOWLIST_CACHE.lock().unwrap();
    *cache = None;
}

/// Persist the enabled subset. Rejects (without writing) any member absent
/// from the constant — exactly as `providers.rs:570` rejects a non-member
/// capability. Persistence is best-effort upsert; a DB failure is surfaced
/// as a logged warning but the validation gate is the security boundary.
///
/// Kept for backward compatibility and test coverage; the PUT handler now
/// uses [`set_enabled_allowlist_checked`] (strict, propagates errors).
#[allow(dead_code)]
pub async fn set_enabled_allowlist(
    db: &PgPool,
    members: &[String],
) -> Result<(), AllowlistError> {
    validate_allowlist_toggle(members)?;
    if let Err(e) =
        opex_db::sys_flags::upsert(db, ALLOWLIST_FLAG_KEY, serde_json::json!(members)).await
    {
        tracing::warn!(error = %e, "failed to persist fse allowlist toggle");
    }
    invalidate_allowlist_cache();
    Ok(())
}

/// Strict reader for the PUT path: distinguishes "flag unset" from "DB error".
///
/// * `Ok(None)` → flag not yet written, returns the full `FSE_DEFAULT_ALLOWLIST`.
/// * `Ok(Some(v))` → persisted list, filtered through the constant (stale entries
///   silently dropped for defense-in-depth).
/// * `Err(e)` → SQL failure — propagated so the caller can return 500 BEFORE
///   mutating anything (fixes the fail-open RMW bug in `api_set_allowlist`).
pub async fn get_enabled_allowlist_strict(db: &PgPool) -> anyhow::Result<Vec<String>> {
    let raw: Option<serde_json::Value> =
        opex_db::sys_flags::try_get(db, ALLOWLIST_FLAG_KEY).await?;
    let stored: Option<Vec<String>> = raw.and_then(|v| serde_json::from_value(v).ok());
    Ok(match stored {
        Some(list) => list
            .into_iter()
            .filter(|m| FSE_DEFAULT_ALLOWLIST.contains(&m.as_str()))
            .collect(),
        None => FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect(),
    })
}

/// Checked writer for the PUT path: validates then persists, propagating errors.
///
/// Unlike `set_enabled_allowlist` (best-effort, swallows the upsert error),
/// this variant returns `Err` on both validation failure and DB failure.
/// The audit call in `api_set_allowlist` therefore only fires inside the `Ok`
/// arm — ensuring the audit trail matches the actual committed state.
pub async fn set_enabled_allowlist_checked(
    db: &PgPool,
    members: Vec<String>,
) -> anyhow::Result<()> {
    validate_allowlist_toggle(&members)
        .map_err(|e| anyhow::anyhow!("allowlist validation failed: {e}"))?;
    opex_db::sys_flags::upsert(db, ALLOWLIST_FLAG_KEY, serde_json::json!(members)).await?;
    invalidate_allowlist_cache();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn defaults_to_full_constant_when_unset(pool: sqlx::PgPool) {
        let enabled = get_enabled_allowlist(&pool).await;
        let mut got = enabled.clone();
        got.sort();
        let mut want: Vec<String> =
            FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn set_then_get_roundtrips_a_valid_subset(pool: sqlx::PgPool) {
        let subset = vec!["transcribe".to_string(), "save".to_string()];
        set_enabled_allowlist(&pool, &subset).await.unwrap();
        let mut got = get_enabled_allowlist(&pool).await;
        got.sort();
        assert_eq!(got, vec!["save".to_string(), "transcribe".to_string()]);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn set_rejects_non_constant_member(pool: sqlx::PgPool) {
        let bad = vec!["transcribe".to_string(), "code_exec".to_string()];
        let err = set_enabled_allowlist(&pool, &bad).await.unwrap_err();
        assert!(matches!(err, AllowlistError::UnknownMember(ref a) if a == "code_exec"));
        // and nothing was persisted — still defaults to full constant
        assert_eq!(
            get_enabled_allowlist(&pool).await.len(),
            FSE_DEFAULT_ALLOWLIST.len()
        );
    }
}
