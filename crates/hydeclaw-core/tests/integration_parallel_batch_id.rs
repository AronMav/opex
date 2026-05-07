//! T3 integration tests for ParallelBatchId.
//!
//! Verifies migration 047 schema-level expectations:
//!   1. `messages.parallel_batch_id` column exists and is UUID type
//!   2. Partial index `messages_parallel_batch_idx` exists
//!
//! Behavioral end-to-end batch-grouping verification (multi-tool turn
//! assigns one ParallelBatchId, single-tool turn leaves NULL) is out of
//! scope for the schema test — it requires the LLM mock harness and is
//! covered by Pi-e2e tests after deploy.
//!
//! See:
//!   - migrations/047_messages_parallel_batch_id.sql
//!   - docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md (T3)

use sqlx::PgPool;

#[sqlx::test]
async fn test_migration_047_creates_parallel_batch_id_column(pool: PgPool) {
    // Verify the column exists and has expected type
    let result: (String,) = sqlx::query_as(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_name = 'messages' AND column_name = 'parallel_batch_id'"
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(result.0, "uuid", "parallel_batch_id must be UUID type");
}

#[sqlx::test]
async fn test_migration_047_creates_partial_index(pool: PgPool) {
    // Verify the partial index exists
    let count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM pg_indexes \
         WHERE tablename = 'messages' \
         AND indexname = 'messages_parallel_batch_idx'"
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(count.0, 1, "messages_parallel_batch_idx must exist");
}
