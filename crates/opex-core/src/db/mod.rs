// ── Extracted to opex-db (leaf modules, zero crate::* refs) ─────────
pub use opex_db::approvals;
pub use opex_db::memory_queries;
pub use opex_db::notifications;
pub use opex_db::session_failures;
pub use opex_db::session_timeline;
pub use opex_db::sessions;
pub use opex_db::shares;
pub use opex_db::usage;

// ── Remaining modules (not extracted) ───────────────────────────────────
pub mod access;
pub mod audit_queue;
pub mod audit;
pub mod channel_voice_modes;
pub mod compaction;
pub mod curator_runs;
pub mod github;
pub mod handler_config;
pub mod model_overrides;
pub mod outbound;
pub mod pending;
pub mod providers;
pub mod session_goals;
pub mod skill_repairs;
pub mod curator_decisions;
pub mod skill_versions;
pub mod todos;
pub mod tool_audit;
pub mod tool_quality;
pub mod upload_migration;
pub mod uploads;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::{Duration, Instant};

/// How long to keep retrying the initial database connection at startup before
/// giving up. A transient outage — Postgres being recreated/restarted during a
/// deploy, a network blip, the DB container still in its startup window — must
/// not crash the process. We retry with exponential backoff until the database
/// accepts connections or this window elapses. Sized well above the observed
/// worst case (a `docker compose` recreate leaves Postgres unavailable for
/// ~60s) with margin.
const STARTUP_CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

/// Create the Postgres pool, retrying the initial connection so a database that
/// is briefly unavailable at startup does not take the whole process down.
///
/// On success the returned pool has its `min_connections` already established
/// (the DB is provably reachable). On persistent failure (DB unreachable for
/// the entire [`STARTUP_CONNECT_TIMEOUT`] window) the error is propagated so a
/// genuine misconfiguration still surfaces — just after a bounded wait rather
/// than instantly.
pub async fn create_pool(url: &str) -> Result<PgPool> {
    create_pool_with_timeout(url, STARTUP_CONNECT_TIMEOUT).await
}

async fn create_pool_with_timeout(url: &str, window: Duration) -> Result<PgPool> {
    let options = PgPoolOptions::new()
        .max_connections(30)
        .min_connections(3)
        .acquire_timeout(Duration::from_secs(5));

    let deadline = Instant::now() + window;
    let mut backoff = Duration::from_millis(500);
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        match options.clone().connect(url).await {
            Ok(pool) => {
                if attempt > 1 {
                    tracing::info!(attempt, "database connection established after retry");
                }
                return Ok(pool);
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err).context(format!(
                        "database unreachable after {attempt} attempt(s) over {window:?}"
                    ));
                }
                tracing::warn!(
                    attempt,
                    error = %err,
                    backoff_ms = backoff.as_millis(),
                    "database not ready at startup, retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unreachable database must be retried and then fail with a clear
    /// error inside the bounded window — not hang forever and not crash on the
    /// first refused connection. Uses a tiny window so the retry loop makes a
    /// couple of attempts and then gives up quickly.
    #[tokio::test]
    async fn create_pool_retries_then_fails_on_unreachable_db() {
        let start = Instant::now();
        // 127.0.0.1:1 — nothing listening → connection refused on every attempt.
        let result =
            create_pool_with_timeout("postgres://x:x@127.0.0.1:1/none", Duration::from_millis(700))
                .await;

        assert!(result.is_err(), "unreachable DB must ultimately error");
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("database unreachable after"),
            "error should report the bounded retry window, got: {err}"
        );
        // Bounded: gave up near the window, not instantly (proves it retried)
        // and not unboundedly (proves the deadline is honoured).
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(400),
            "should have retried at least once (elapsed {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(20),
            "should not exceed the window by much (elapsed {elapsed:?})"
        );
    }
}
