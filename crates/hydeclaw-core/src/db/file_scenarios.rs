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
