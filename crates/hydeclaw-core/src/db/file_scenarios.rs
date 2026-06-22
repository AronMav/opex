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
}
