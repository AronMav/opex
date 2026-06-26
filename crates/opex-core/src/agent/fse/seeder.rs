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

/// One default binding row. Pure data — no DB, no async — so the guard test
/// can assert the action names without a live Postgres instance.
#[allow(dead_code)] // fields consumed by reconcile_tests (cfg(test)) and future Phase 5+ callers
pub(crate) struct SeedRow {
    pub match_type: &'static str,
    /// Built-in ACTION name from the in-core dispatch table
    /// (`FSE_DEFAULT_ALLOWLIST`). Must NOT be a YAML-tool alias
    /// (`transcribe_audio`, `analyze_image`, …) — the dispatch table only
    /// recognises the canonical names; a YAML alias would fail-closed to
    /// `ScenarioStatus::Unsupported` at run time.
    pub action_ref: &'static str,
    pub label: &'static str,
    /// Always `"tool"` for default bindings (security requirement).
    pub executor: &'static str,
    pub is_default: bool,
    pub priority: i32,
}

/// The canonical default seed rows in pure-data form. Every `action_ref` here
/// is a built-in dispatch-table key, cross-checked in
/// `reconcile_tests::seed_uses_builtin_action_names`.
///
/// Adding a new row here requires a parallel entry in `FSE_DEFAULT_ALLOWLIST`
/// (the compiler won't enforce that — the guard test will catch it at CI).
pub(crate) fn default_seed_rows() -> Vec<SeedRow> {
    vec![
        SeedRow {
            match_type: "audio/*",
            action_ref: "transcribe",
            label: "Распознать речь",
            executor: "tool",
            is_default: true,
            priority: 100,
        },
        SeedRow {
            match_type: "image/*",
            action_ref: "describe",
            label: "Описать изображение",
            executor: "tool",
            is_default: true,
            priority: 100,
        },
        SeedRow {
            match_type: "application/pdf",
            action_ref: "extract_document",
            label: "Извлечь текст документа",
            executor: "tool",
            is_default: true,
            priority: 100,
        },
        SeedRow {
            match_type: "video/*",
            action_ref: "summarize_video",
            label: "Сводка видео",
            executor: "tool",
            is_default: true,
            priority: 100,
        },
    ]
}

/// Insert the default bindings if absent. Returns the number of rows
/// actually inserted (0 on a re-seed). Safe to call on every startup.
pub async fn seed_default_file_scenarios(db: &PgPool) -> anyhow::Result<u64> {
    let mut inserted: u64 = 0;
    for row in default_seed_rows() {
        // Defense-in-depth: never seed a default that is not in the allowlist
        // constant (keeps the seeder honest against a future careless edit).
        debug_assert!(
            FSE_DEFAULT_ALLOWLIST.contains(&row.action_ref),
            "seed action '{}' must be in FSE_DEFAULT_ALLOWLIST",
            row.action_ref
        );
        let res = sqlx::query(
            "INSERT INTO file_scenarios \
             (id, match_type, executor, action_ref, label, is_default, priority, enabled, scope, created_by) \
             VALUES (gen_random_uuid(), $1, 'tool', $2, $3, true, 100, true, 'global', 'system') \
             ON CONFLICT (match_type, action_ref) DO NOTHING",
        )
        .bind(row.match_type)
        .bind(row.action_ref)
        .bind(row.label)
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
        assert_eq!(inserted, 4);
        assert_eq!(count(&pool).await, 4);

        // exactly one default per seeded match_type, all executor='tool'
        let defaults: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM file_scenarios WHERE is_default AND executor = 'tool'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(defaults, 4);

        // the seeded match_types are present
        for mt in ["audio/*", "image/*", "application/pdf", "video/*"] {
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
        assert_eq!(seed_default_file_scenarios(&pool).await.unwrap(), 4);
        // second run inserts nothing, does not error, leaves 4 rows
        assert_eq!(seed_default_file_scenarios(&pool).await.unwrap(), 0);
        assert_eq!(count(&pool).await, 4);
    }
}

#[cfg(test)]
mod reconcile_tests {
    use super::*;

    /// Regression guard: every seeded default row must carry a built-in
    /// dispatch-table action name (`FSE_DEFAULT_ALLOWLIST`), NEVER a YAML-tool
    /// alias (`transcribe_audio`, `analyze_image`, …). The dispatch table is
    /// keyed by action names; a YAML alias resolves to `None` → `Unsupported`
    /// at run time, silently breaking the deterministic default.
    ///
    /// This test is intentionally pure (no DB) so it runs without DATABASE_URL.
    #[test]
    fn seed_uses_builtin_action_names() {
        let rows = default_seed_rows();
        assert!(!rows.is_empty(), "seed must produce at least one row");

        // Names that LOOK plausible but are YAML-tool aliases, not dispatch keys.
        let yaml_aliases = [
            "transcribe_audio",
            "analyze_image",
            "extract_document_tool",
            "describe_image",
        ];

        for row in &rows {
            assert_eq!(row.executor, "tool", "seeded defaults must be executor='tool'");
            assert!(row.is_default, "seeded rows must have is_default=true");
            assert!(
                FSE_DEFAULT_ALLOWLIST.contains(&row.action_ref),
                "action_ref {:?} is not in FSE_DEFAULT_ALLOWLIST ({:?}); \
                 use the built-in dispatch key, not a YAML-tool alias",
                row.action_ref,
                FSE_DEFAULT_ALLOWLIST,
            );
            assert!(
                !yaml_aliases.contains(&row.action_ref),
                "action_ref {:?} is a YAML-tool alias — seed must use the \
                 built-in dispatch-table name instead",
                row.action_ref,
            );
        }

        // The three intentional default match-types are all present.
        let types: Vec<&str> = rows.iter().map(|r| r.match_type).collect();
        assert!(types.contains(&"audio/*"), "audio/* default is missing from seed");
        assert!(types.contains(&"image/*"), "image/* default is missing from seed");
        assert!(
            types.iter().any(|t| t.contains("pdf") || *t == "application/pdf"),
            "application/pdf default is missing from seed",
        );
    }

    #[test]
    fn seed_includes_video_default() {
        let rows = default_seed_rows();
        assert!(
            rows.iter().any(|r| r.match_type == "video/*" && r.action_ref == "summarize_video"),
            "video/* → summarize_video default must be seeded",
        );
    }
}
