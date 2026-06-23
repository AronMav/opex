//! Embedded migration runner. Kept in its own file so multiple harness shapes
//! (full TestHarness, future SQL-only fixtures) can share it without re-embedding.

use anyhow::Result;
use sqlx::PgPool;

/// Apply every migration in `migrations/` against the given pool.
/// Path is relative to this crate (`crates/opex-core/`), so `../../migrations`.
pub async fn apply_all(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .map_err(|e| anyhow::anyhow!("migrations failed: {e}"))
}
