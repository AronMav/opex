//! `file_scenarios` table CRUD. See docs/superpowers/specs/2026-06-22-file-scenario-engine-design.md §4.1.

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

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
