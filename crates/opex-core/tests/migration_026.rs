#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn migration_026_idempotent_on_second_run(pool: PgPool) {
    // Seed a provider with legacy flat timeout.
    sqlx::query(
        "INSERT INTO providers (id, name, type, provider_type, enabled, options)
         VALUES (gen_random_uuid(), 'legacy', 'llm', 'openai', true,
                 '{\"timeout_secs\": 45}'::jsonb)",
    )
    .execute(&pool).await.unwrap();

    // The framework already ran migrations; simulate a second run by
    // re-executing 026 explicitly.
    let sql = std::fs::read_to_string("../../migrations/026_provider_timeouts_nested.sql").unwrap();
    sqlx::raw_sql(&sql).execute(&pool).await.unwrap();

    // Operator edit BETWEEN runs: change request_secs to 30 manually.
    sqlx::query(
        "UPDATE providers SET options = jsonb_set(options, '{timeouts,request_secs}', '30'::jsonb) WHERE name='legacy'"
    ).execute(&pool).await.unwrap();

    // Third run — must NOT overwrite the 30.
    sqlx::raw_sql(&sql).execute(&pool).await.unwrap();

    let v: serde_json::Value = sqlx::query_scalar(
        "SELECT options FROM providers WHERE name='legacy'"
    ).fetch_one(&pool).await.unwrap();

    assert_eq!(v["timeouts"]["request_secs"], serde_json::json!(30));
    assert_eq!(v["timeouts"]["connect_secs"], serde_json::json!(10));
    assert!(v.get("timeout_secs").is_none(), "legacy key must be gone");
}

#[sqlx::test(migrations = "../../migrations")]
async fn migration_026_preserves_timeout_zero(pool: PgPool) {
    sqlx::query(
        "INSERT INTO providers (id, name, type, provider_type, enabled, options)
         VALUES (gen_random_uuid(), 'unlimited', 'llm', 'openai', true,
                 '{\"timeout_secs\": 0}'::jsonb)",
    )
    .execute(&pool).await.unwrap();

    let sql = std::fs::read_to_string("../../migrations/026_provider_timeouts_nested.sql").unwrap();
    sqlx::raw_sql(&sql).execute(&pool).await.unwrap();

    let req: i64 = sqlx::query_scalar(
        "SELECT (options->'timeouts'->>'request_secs')::bigint FROM providers WHERE name='unlimited'"
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(req, 0);

    let flag: serde_json::Value = sqlx::query_scalar(
        "SELECT value FROM system_flags WHERE key='v020_providers_with_no_request_limit'"
    ).fetch_one(&pool).await.unwrap();
    assert!(flag.as_array().unwrap().iter().any(|n| n == "unlimited"));
}

/// Issue #3: when a provider row has BOTH legacy `timeout_secs` AND a
/// hand-edited `timeouts` object (e.g. operator tuned things between a
/// partially-migrated snapshot and the next startup), the migration MUST
/// preserve the hand-edited `timeouts` verbatim and just strip the orphan
/// `timeout_secs`. The previous single-UPDATE form clobbered the
/// `timeouts` object by unconditionally rebuilding it from defaults +
/// `timeout_secs`.
#[sqlx::test(migrations = "../../migrations")]
async fn migration_026_preserves_hand_edited_timeouts_with_legacy_key(pool: PgPool) {
    // Seed: mixed-keys row. `timeout_secs: 999` is the legacy poison,
    // `timeouts` is hand-tuned — request_secs 37 (NOT 999, NOT the 120
    // default), connect_secs 42, and two non-default stream tiers.
    sqlx::query(
        "INSERT INTO providers (id, name, type, provider_type, enabled, options)
         VALUES (gen_random_uuid(), 'mixed', 'llm', 'openai', true,
                 '{
                    \"timeout_secs\": 999,
                    \"timeouts\": {
                        \"connect_secs\": 42,
                        \"request_secs\": 37,
                        \"stream_inactivity_secs\": 77,
                        \"stream_max_duration_secs\": 888
                    }
                 }'::jsonb)",
    )
    .execute(&pool).await.unwrap();

    // Re-run migration explicitly (the framework ran it once already,
    // but the row was inserted *after* initial migrations applied — so
    // this run is what actually processes the poisoned row).
    let sql = std::fs::read_to_string("../../migrations/026_provider_timeouts_nested.sql").unwrap();
    sqlx::raw_sql(&sql).execute(&pool).await.unwrap();

    let v: serde_json::Value = sqlx::query_scalar(
        "SELECT options FROM providers WHERE name='mixed'"
    ).fetch_one(&pool).await.unwrap();

    // Hand-edited `timeouts` preserved verbatim across all four tiers.
    assert_eq!(
        v["timeouts"]["connect_secs"],
        serde_json::json!(42),
        "hand-edited connect_secs MUST NOT be overwritten to the 10 default"
    );
    assert_eq!(
        v["timeouts"]["request_secs"],
        serde_json::json!(37),
        "hand-edited request_secs MUST NOT be clobbered by timeout_secs=999"
    );
    assert_eq!(
        v["timeouts"]["stream_inactivity_secs"],
        serde_json::json!(77),
        "hand-edited stream_inactivity_secs preserved"
    );
    assert_eq!(
        v["timeouts"]["stream_max_duration_secs"],
        serde_json::json!(888),
        "hand-edited stream_max_duration_secs preserved"
    );

    // Orphan `timeout_secs` stripped.
    assert!(
        v.get("timeout_secs").is_none(),
        "legacy timeout_secs key must be gone: {v}"
    );

    // Running the migration again on the already-clean row must be a no-op.
    sqlx::raw_sql(&sql).execute(&pool).await.unwrap();
    let v2: serde_json::Value = sqlx::query_scalar(
        "SELECT options FROM providers WHERE name='mixed'"
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(v, v2, "second run must be a no-op on an already-clean row");
}
