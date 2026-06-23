//! Ephemeral PostgreSQL test harness.
//!
//! Spawns a fresh `pgvector/pgvector:pg17` container per `TestHarness::new()` call
//! (matches production image — see CONTEXT.md decision), applies every migration
//! in `migrations/`, and exposes a connected pool.
//!
//! On Drop the underlying `ContainerAsync` is dropped, which removes the container.
//!
//! Override the image via `HYDECLAW_PG_TEST_IMAGE=<repo>:<tag>` (split on the LAST
//! `:` so registry hosts with explicit ports still parse). Examples:
//!   - HYDECLAW_PG_TEST_IMAGE=postgres:17                 (vanilla PG, no pgvector)
//!   - HYDECLAW_PG_TEST_IMAGE=ghcr.io/foo/pg:17-age       (custom registry)

use anyhow::{Context, Result};
use sqlx::PgPool;
use testcontainers::core::{ContainerRequest, IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// Env var: full `image:tag` override. CONTEXT.md decision-locked name.
const PG_IMAGE_ENV: &str = "HYDECLAW_PG_TEST_IMAGE";
/// Default image — matches production deployment. CONTEXT.md decision-locked value.
const DEFAULT_PG_IMAGE: &str = "pgvector/pgvector:pg17";

pub struct TestHarness {
    // Order matters: `pool` must drop before `_container` so connections
    // close cleanly before the container is torn down.
    pool: PgPool,
    pg_url: String,
    _container: ContainerAsync<GenericImage>,
}

/// Parse `image_spec` into `(repo, tag)` by splitting on the LAST `:`.
/// This handles registry hosts with explicit ports, e.g.
/// `registry.example.com:5000/pg:17` → `("registry.example.com:5000/pg", "17")`.
fn parse_image_spec(image_spec: &str) -> Result<(String, String)> {
    match image_spec.rsplit_once(':') {
        Some((r, t)) if !r.is_empty() && !t.is_empty() => Ok((r.to_string(), t.to_string())),
        _ => anyhow::bail!(
            "{} must be of the form '<image>:<tag>', got: {:?}",
            PG_IMAGE_ENV,
            image_spec
        ),
    }
}

/// Build a base `ContainerRequest<GenericImage>` for the given repo/tag,
/// ready for PG env-var injection and optional extra configuration.
fn base_pg_image(repo: &str, tag: &str) -> ContainerRequest<GenericImage> {
    GenericImage::new(repo, tag)
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "postgres")
}

impl TestHarness {
    /// Spin up a fresh PG container, run all migrations, return a connected harness.
    pub async fn new() -> Result<Self> {
        let image_spec = std::env::var(PG_IMAGE_ENV)
            .unwrap_or_else(|_| DEFAULT_PG_IMAGE.to_string());
        let (repo, tag) = parse_image_spec(&image_spec)?;

        let container = base_pg_image(&repo, &tag)
            .start()
            .await
            .with_context(|| format!("starting ephemeral PostgreSQL container ({image_spec})"))?;

        Self::from_container(container).await
    }

    /// Spin up a fresh PG container with `shared_preload_libraries=pg_stat_statements`
    /// active, apply migrations, and enable the extension via `CREATE EXTENSION IF NOT EXISTS`.
    ///
    /// Phase 63 DATA-03 uses this variant to verify batch INSERT round-trip counts
    /// against `pg_stat_statements.calls`. Plain `TestHarness::new()` does NOT preload
    /// the extension, so the view is not queryable there.
    ///
    /// Implementation: we override the container CMD via `ImageExt::with_cmd(["postgres",
    /// "-c", "shared_preload_libraries=pg_stat_statements", "-c", "pg_stat_statements.track=all"])`.
    /// This is the documented testcontainers-rs way to pass `-c key=value` to a
    /// pre-built Postgres image without rebuilding the image itself.
    pub async fn new_with_pg_stat_statements() -> Result<Self> {
        let image_spec = std::env::var(PG_IMAGE_ENV)
            .unwrap_or_else(|_| DEFAULT_PG_IMAGE.to_string());
        let (repo, tag) = parse_image_spec(&image_spec)?;

        let container = base_pg_image(&repo, &tag)
            .with_cmd(vec![
                "postgres".to_string(),
                "-c".to_string(),
                "shared_preload_libraries=pg_stat_statements".to_string(),
                "-c".to_string(),
                "pg_stat_statements.track=all".to_string(),
            ])
            .start()
            .await
            .with_context(|| {
                format!("starting ephemeral PG with pg_stat_statements ({image_spec})")
            })?;

        let harness = Self::from_container(container).await?;

        // Extension must exist at the connection level to populate the view.
        sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
            .execute(&harness.pool)
            .await
            .context("creating pg_stat_statements extension on ephemeral PG")?;

        Ok(harness)
    }

    /// Shared container-to-harness wiring: resolve host port, connect pool, apply migrations.
    async fn from_container(container: ContainerAsync<GenericImage>) -> Result<Self> {
        let host_port = container
            .get_host_port_ipv4(5432)
            .await
            .context("resolving container host port")?;
        let pg_url = format!("postgres://postgres:postgres@127.0.0.1:{host_port}/postgres");

        let pool = PgPool::connect(&pg_url)
            .await
            .context("connecting to ephemeral PG")?;

        super::migrations::apply_all(&pool)
            .await
            .context("applying migrations to ephemeral PG")?;

        Ok(Self {
            pool,
            pg_url,
            _container: container,
        })
    }

    /// Borrow the connected pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// PostgreSQL URL of the ephemeral container.
    pub fn pg_url(&self) -> &str {
        &self.pg_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Testcontainers requires a reachable Docker daemon. CI runners that
    // lack one — Windows, macOS, and `uraimo/run-on-arch-action` (QEMU,
    // no nested Docker) — fail to pull the pgvector image. Ignore by
    // default; x86_64 Linux CI lanes opt in with `cargo test -- --ignored`.
    #[cfg_attr(not(all(target_os = "linux", target_arch = "x86_64")), ignore = "testcontainers needs Docker daemon (x86_64 Linux only)")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pg_stat_statements_variant_exposes_extension() {
        let harness = TestHarness::new_with_pg_stat_statements()
            .await
            .expect("harness must come up with pg_stat_statements preloaded");
        let pool = harness.pool();

        let extension_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM pg_extension WHERE extname = 'pg_stat_statements'",
        )
        .fetch_one(pool)
        .await
        .expect("pg_extension query");
        assert_eq!(
            extension_count.0, 1,
            "pg_stat_statements extension must be installed on the new variant"
        );

        // The view must be queryable (proves shared_preload_libraries was effective).
        let _sample: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM pg_stat_statements")
            .fetch_one(pool)
            .await
            .expect("pg_stat_statements view must be queryable when the library is preloaded");
    }

    #[cfg_attr(not(all(target_os = "linux", target_arch = "x86_64")), ignore = "testcontainers needs Docker daemon (x86_64 Linux only)")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plain_variant_does_not_preload_pg_stat_statements() {
        let harness = TestHarness::new().await.expect("plain harness");
        let pool = harness.pool();
        let extension_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM pg_extension WHERE extname = 'pg_stat_statements'",
        )
        .fetch_one(pool)
        .await
        .expect("pg_extension query");
        assert_eq!(
            extension_count.0, 0,
            "plain TestHarness::new() MUST NOT install pg_stat_statements — it is opt-in"
        );
    }
}
