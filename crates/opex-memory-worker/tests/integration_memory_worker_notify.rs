//! Phase 66 REF-04 integration tests for LISTEN/NOTIFY + poll safety-net.
//!
//! Scenario A (happy path): INSERT one task_type='test_noop' row → assert the
//! worker claims it in <100 ms (LISTEN path — proves sub-100ms pickup).
//!
//! Scenario B (crash recovery): INSERT 50 rows in a single transaction → wait
//! until at least 20 have reached 'processing' or 'done' → kill -9 the child →
//! restart the worker → wait until all 50 have status='done'. Asserts
//! `COUNT(*) WHERE status='done' = 50` AND `COUNT WHERE status IN ('pending',
//! 'processing','failed') = 0`.
//!
//! Gating:
//! - Requires the `test-noop` crate feature (adds a short-circuit dispatch arm
//!   for `task_type='test_noop'`).
//! - Requires Docker daemon (testcontainers spawns pgvector/pgvector:pg17).
//!   Skipped on hosts without Docker — the test compiles but cannot run.
//! - Ignored on Windows via `#[cfg_attr(windows, ignore)]` — tokio's
//!   `Command::kill` sends `TerminateProcess`, but the named-pipe process
//!   control is flakier than on Unix; CI runs this on Linux x86_64 + aarch64.
//!
//! Harness is self-contained in this file (≈80 lines) — the memory-worker
//! crate does not pull in the opex-core `tests/support/` tree.

#![cfg(feature = "test-noop")]

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

const PG_IMAGE: &str = "pgvector/pgvector:pg17";
const MIGRATIONS_DIR: &str = "../../migrations";
/// Happy-path latency budget: NOTIFY wake should beat the 5s poll by ~50x.
const PICKUP_LATENCY_BUDGET_MS: u128 = 100;
/// Crash-recovery wall-clock budget — generous for CI on slow aarch64 runners.
const CRASH_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Burst size for Scenario B.
const BURST_SIZE: i64 = 50;

struct PgHarness {
    _container: ContainerAsync<GenericImage>,
    pool: PgPool,
    pg_url: String,
}

impl PgHarness {
    async fn new() -> Result<Self> {
        let image = GenericImage::new("pgvector/pgvector", "pg17")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ));
        let container = image
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_DB", "postgres")
            .start()
            .await
            .with_context(|| format!("starting {PG_IMAGE}"))?;

        let host_port = container
            .get_host_port_ipv4(5432)
            .await
            .context("resolving PG host port")?;
        let pg_url = format!("postgres://postgres:postgres@127.0.0.1:{host_port}/postgres");

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&pg_url)
            .await
            .context("connecting to ephemeral PG")?;

        // Run migrations (including 023 which installs the NOTIFY trigger).
        sqlx::migrate::Migrator::new(std::path::Path::new(MIGRATIONS_DIR))
            .await
            .context("loading migrations from ../../migrations")?
            .run(&pool)
            .await
            .context("applying migrations")?;

        Ok(Self {
            _container: container,
            pool,
            pg_url,
        })
    }
}

/// Write a minimal opex.toml to a temp dir so the worker can `load_config`.
fn write_worker_config(tempdir: &std::path::Path, pg_url: &str) -> Result<PathBuf> {
    let path = tempdir.join("opex.toml");
    let body = format!(
        r#"
[database]
url = "{pg_url}"

[memory_worker]
enabled = true
poll_interval_secs = 1
notify_mode = "listen"

[memory]
workspace_dir = "{}"
"#,
        tempdir.display().to_string().replace('\\', "/"),
    );
    std::fs::write(&path, body).context("writing worker config")?;
    // Ensure workspace dir exists (reindex handler would touch it, but
    // test_noop bypasses that — safety net regardless).
    let _ = std::fs::create_dir_all(tempdir);
    Ok(path)
}

/// Spawn the memory-worker binary pointed at the ephemeral PG.
///
/// Uses `env!("CARGO_BIN_EXE_opex-memory-worker")` so the compiled test
/// binary locates the sibling binary artifact without path-hunting.
async fn spawn_worker(config_path: &std::path::Path) -> Result<Child> {
    let bin = env!("CARGO_BIN_EXE_opex-memory-worker");
    let child = Command::new(bin)
        .arg(config_path)
        .env("RUST_LOG", "opex_memory_worker=info")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawning memory-worker binary")?;
    Ok(child)
}

/// Wait until the worker logs "LISTEN memory_tasks_new active" on stderr, or
/// the fallback "memory worker starting" + a brief sleep. Bounded to 10s.
async fn wait_for_ready(child: &mut Child) -> Result<()> {
    let stderr = child.stderr.take().context("child stderr missing")?;
    let mut reader = BufReader::new(stderr).lines();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = tokio::time::timeout(remaining, reader.next_line()).await;
        match line {
            Ok(Ok(Some(l))) => {
                if l.contains("LISTEN memory_tasks_new active") {
                    // Put the reader back so subsequent output isn't dropped —
                    // done implicitly when `reader` drops at function end.
                    return Ok(());
                }
            }
            Ok(Ok(None)) => anyhow::bail!("worker stderr closed before ready signal"),
            Ok(Err(e)) => anyhow::bail!("stderr read error: {e}"),
            Err(_) => anyhow::bail!("timeout waiting for worker ready signal"),
        }
    }
    anyhow::bail!("worker did not emit ready signal within 10s")
}

// ── Scenario A: sub-100ms happy-path pickup latency ────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(windows, ignore = "tokio Command::kill is flaky on Windows named pipes")]
async fn happy_path_pickup_latency_under_100ms() -> Result<()> {
    let harness = PgHarness::new().await.expect("testcontainer PG");
    let tmp = tempfile::tempdir().context("tempdir")?;
    let cfg = write_worker_config(tmp.path(), &harness.pg_url)?;

    let mut worker = spawn_worker(&cfg).await?;
    wait_for_ready(&mut worker).await?;

    // Enqueue a single task_type='test_noop' row and record t0 at commit.
    let t0 = Instant::now();
    let task_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO memory_tasks (task_type, params) VALUES ('test_noop', '{}'::jsonb) RETURNING id",
    )
    .fetch_one(&harness.pool)
    .await
    .context("enqueue test_noop")?;

    // Poll DB every 5ms until status != 'pending' — record latency.
    let mut picked_up_at: Option<Instant> = None;
    for _ in 0..400u32 {
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM memory_tasks WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(&harness.pool)
        .await?;
        match status.as_deref() {
            Some("pending") | None => tokio::time::sleep(Duration::from_millis(5)).await,
            Some(_other) => {
                picked_up_at = Some(Instant::now());
                break;
            }
        }
    }

    let picked_up_at = picked_up_at.expect("task must transition out of 'pending' within 2s");
    let latency_ms = picked_up_at.duration_since(t0).as_millis();

    // Clean kill before assertion so failure doesn't leak the child.
    let _ = worker.kill().await;

    assert!(
        latency_ms < PICKUP_LATENCY_BUDGET_MS,
        "happy-path pickup latency {latency_ms}ms exceeds 100ms budget — LISTEN/NOTIFY regression?"
    );
    Ok(())
}

// ── Scenario B: kill -9 during burst → 100% completion after restart ────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(windows, ignore = "tokio Command::kill is flaky on Windows named pipes")]
async fn crash_during_burst_still_processes_all_tasks() -> Result<()> {
    let harness = PgHarness::new().await.expect("testcontainer PG");
    let tmp = tempfile::tempdir().context("tempdir")?;
    let cfg = write_worker_config(tmp.path(), &harness.pg_url)?;

    let mut worker = spawn_worker(&cfg).await?;
    wait_for_ready(&mut worker).await?;

    // Insert BURST_SIZE rows in a single transaction — NOTIFY fires at COMMIT
    // so all 50 wakeups may coalesce into a single recv() at the worker side.
    // The worker's drain-loop is responsible for processing all of them.
    {
        let mut tx = harness.pool.begin().await?;
        for _ in 0..BURST_SIZE {
            sqlx::query("INSERT INTO memory_tasks (task_type, params) VALUES ('test_noop', '{}'::jsonb)")
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }

    // Wait until at least 20 rows have been claimed (processing or done).
    let mut in_flight = 0i64;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        in_flight = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM memory_tasks WHERE status IN ('processing', 'done')",
        )
        .fetch_one(&harness.pool)
        .await?;
        if in_flight >= 20 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        in_flight >= 20,
        "worker should have claimed at least 20 rows before kill; got {in_flight}"
    );

    // Simulate kill -9 — tokio's Child::kill().await sends SIGKILL on Unix.
    worker.kill().await.context("killing worker mid-burst")?;
    let _ = worker.wait().await;

    // Restart the worker — recover_stuck() resets any 'processing' rows to
    // 'pending', the LISTEN loop wakes (or the poll safety net fires) and
    // drain-loop completes the remaining work.
    let mut worker2 = spawn_worker(&cfg).await?;
    wait_for_ready(&mut worker2).await?;

    // Poll until all 50 are done (or timeout).
    let deadline = Instant::now() + CRASH_RECOVERY_TIMEOUT;
    let mut done = 0i64;
    while Instant::now() < deadline {
        done = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM memory_tasks WHERE status = 'done'",
        )
        .fetch_one(&harness.pool)
        .await?;
        if done == BURST_SIZE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let pending_etc: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM memory_tasks WHERE status IN ('pending', 'processing', 'failed')",
    )
    .fetch_one(&harness.pool)
    .await?;

    // Clean up before asserting so a failure doesn't leak the second child.
    let _ = worker2.kill().await;

    assert_eq!(
        done, BURST_SIZE,
        "all {BURST_SIZE} tasks must complete after kill -9 + restart; got done={done}"
    );
    assert_eq!(
        pending_etc, 0,
        "no tasks should remain pending/processing/failed after recovery"
    );
    Ok(())
}
