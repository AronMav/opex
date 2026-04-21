// ── Extracted to hydeclaw-db (leaf modules, zero crate::* refs) ─────────
pub use hydeclaw_db::approvals;
pub use hydeclaw_db::notifications;
pub use hydeclaw_db::session_wal;
pub use hydeclaw_db::sessions;
pub use hydeclaw_db::usage;

// ── Remaining modules (not extracted) ───────────────────────────────────
pub mod access;
pub mod audit_queue;
pub mod audit;
pub mod github;
pub mod memory_queries;
pub mod outbound;
pub mod pending;
pub mod providers;
pub mod skill_metrics;
pub mod skill_versions;
pub mod tool_audit;
pub mod tool_quality;

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub async fn create_pool(url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(30)
        .min_connections(3)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await?;
    Ok(pool)
}
