//! Operator-editable closed-domain allowlist toggle, persisted in the
//! `system_flags` JSONB table under key `fse.allowlist.enabled` as a JSON
//! string array. Unset == "all constant members enabled" (the seeded
//! default). Writes are validated against `FSE_DEFAULT_ALLOWLIST` so the
//! toggle can never admit a non-built-in action (design §4.6).

use sqlx::PgPool;

use super::allowlist::{validate_allowlist_toggle, AllowlistError, FSE_DEFAULT_ALLOWLIST};

/// `system_flags` key holding the JSON array of currently-enabled members.
const ALLOWLIST_FLAG_KEY: &str = "fse.allowlist.enabled";

/// Return the enabled subset of `FSE_DEFAULT_ALLOWLIST`. When the flag is
/// unset (fresh install) or unreadable, defaults to the FULL constant — the
/// seeded defaults must work out of the box. Any stale value not in the
/// constant is silently dropped (defense-in-depth against a hand-edited row).
pub async fn get_enabled_allowlist(db: &PgPool) -> Vec<String> {
    let stored: Option<Vec<String>> = hydeclaw_db::sys_flags::get(db, ALLOWLIST_FLAG_KEY)
        .await
        .and_then(|v| serde_json::from_value(v).ok());
    match stored {
        Some(list) => list
            .into_iter()
            .filter(|m| FSE_DEFAULT_ALLOWLIST.contains(&m.as_str()))
            .collect(),
        None => FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect(),
    }
}

/// Persist the enabled subset. Rejects (without writing) any member absent
/// from the constant — exactly as `providers.rs:570` rejects a non-member
/// capability. Persistence is best-effort upsert; a DB failure is surfaced
/// as a logged warning but the validation gate is the security boundary.
pub async fn set_enabled_allowlist(
    db: &PgPool,
    members: &[String],
) -> Result<(), AllowlistError> {
    validate_allowlist_toggle(members)?;
    if let Err(e) =
        hydeclaw_db::sys_flags::upsert(db, ALLOWLIST_FLAG_KEY, serde_json::json!(members)).await
    {
        tracing::warn!(error = %e, "failed to persist fse allowlist toggle");
    }
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
