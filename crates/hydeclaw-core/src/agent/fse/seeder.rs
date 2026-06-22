//! Idempotent startup seeder for the default `file_scenarios` bindings.
//!
//! Modeled on the memory-reindex first-run bootstrap (`main.rs:409-483`):
//! run once at startup, conflict-safe so re-running on every boot is a no-op.
//! Uses `ON CONFLICT (match_type, action_ref) DO NOTHING` against the
//! `UNIQUE(match_type, action_ref)` constraint rather than a hard migration
//! INSERT (design §4.1), so operator edits to a seeded row are never
//! clobbered on the next restart.

use sqlx::PgPool;

use super::allowlist::FSE_DEFAULT_ALLOWLIST;

/// The three deterministic default tool-bindings. Each reproduces today's
/// behavior (audio→transcribe, image→describe) plus the one intentional new
/// default (document→extract_document, design §3.3/§7). `priority = 100` is
/// the table default; `created_by = 'system'` is the audit tag.
const DEFAULT_BINDINGS: &[(&str, &str, &str)] = &[
    // (match_type, action_ref, label)
    ("audio/*", "transcribe", "Transcribe audio"),
    ("image/*", "describe", "Describe image"),
    ("application/pdf", "extract_document", "Extract document text"),
];

/// Insert the default bindings if absent. Returns the number of rows
/// actually inserted (0 on a re-seed). Safe to call on every startup.
pub async fn seed_default_file_scenarios(db: &PgPool) -> anyhow::Result<u64> {
    let mut inserted: u64 = 0;
    for (match_type, action_ref, label) in DEFAULT_BINDINGS {
        // Defense-in-depth: never seed a default that is not in the allowlist
        // constant (keeps the seeder honest against a future careless edit).
        debug_assert!(
            FSE_DEFAULT_ALLOWLIST.contains(action_ref),
            "seed action '{action_ref}' must be in FSE_DEFAULT_ALLOWLIST"
        );
        let res = sqlx::query(
            "INSERT INTO file_scenarios \
             (id, match_type, executor, action_ref, label, is_default, priority, enabled, scope, created_by) \
             VALUES (gen_random_uuid(), $1, 'tool', $2, $3, true, 100, true, 'global', 'system') \
             ON CONFLICT (match_type, action_ref) DO NOTHING",
        )
        .bind(match_type)
        .bind(action_ref)
        .bind(label)
        .execute(db)
        .await?;
        inserted += res.rows_affected();
    }
    if inserted > 0 {
        tracing::info!(rows = inserted, "seeded default file_scenarios bindings");
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn count(pool: &sqlx::PgPool) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM file_scenarios")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn seeds_three_defaults_on_fresh_db(pool: sqlx::PgPool) {
        let inserted = seed_default_file_scenarios(&pool).await.unwrap();
        assert_eq!(inserted, 3);
        assert_eq!(count(&pool).await, 3);

        // exactly one default per seeded match_type, all executor='tool'
        let defaults: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM file_scenarios WHERE is_default AND executor = 'tool'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(defaults, 3);

        // the seeded match_types are present
        for mt in ["audio/*", "image/*", "application/pdf"] {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM file_scenarios WHERE match_type = $1 AND is_default",
            )
            .bind(mt)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(n, 1, "expected one default for {mt}");
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn is_idempotent_on_reseed(pool: sqlx::PgPool) {
        assert_eq!(seed_default_file_scenarios(&pool).await.unwrap(), 3);
        // second run inserts nothing, does not error, leaves 3 rows
        assert_eq!(seed_default_file_scenarios(&pool).await.unwrap(), 0);
        assert_eq!(count(&pool).await, 3);
    }
}
