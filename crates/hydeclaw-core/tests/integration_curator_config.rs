//! Integration tests for curator config hot-reload.
//!
//! Covers:
//! 1. `config_update_modifies_curator_fields`   — TOML write + re-parse reflects new values.
//! 2. `shared_config_reflects_curator_update`   — `Arc<RwLock<...>>` concurrent read sees live value.
//! 3. `curator_run_returns_none_on_empty_db`    — `last_run` returns `None` on empty DB (DB layer).
//! 4. `curator_status_db_fields`                — insert a run, verify round-trip fields.
//! 5. `get_run_round_trip`                      — get_run by id, None for missing.
//! 6. `list_runs_ordering_and_limit`            — list_runs returns newest-first, respects limit.
//!
//! Tests 1–2 run everywhere (no Docker needed).
//! Tests 3–6 require Docker (sqlx::test spins up a Postgres container) and are
//! gated to Linux x86_64 where Docker is available in CI.

mod support;

use anyhow::Result;

// ── Config-layer helpers (no DB needed) ───────────────────────────────────────

/// Minimum valid TOML content with a `[curator]` section.
fn minimal_config_toml(curator_enabled: bool, curator_cron: &str) -> String {
    format!(
        r#"
[gateway]
listen = "0.0.0.0:18789"

[database]
url = "postgres://localhost/test"

[curator]
enabled = {enabled}
cron = "{cron}"
"#,
        enabled = curator_enabled,
        cron = curator_cron,
    )
}

/// Write `content` to a temp file; dropping the handle deletes the file.
fn write_temp_config(content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().expect("temp file");
    f.write_all(content.as_bytes()).expect("write temp config");
    f.flush().expect("flush");
    f
}

/// Minimal reimplementation of `update_curator_config` using `toml_edit`.
/// Avoids pulling the full `AppConfig` struct into the lib facade
/// (which would cascade `crate::memory` and `crate::process_manager`).
fn update_curator_in_file(
    path: &str,
    enabled: Option<bool>,
    cron: Option<&str>,
) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = content.parse()?;
    if doc.get("curator").is_none() {
        doc["curator"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if let Some(v) = enabled {
        doc["curator"]["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = cron {
        doc["curator"]["cron"] = toml_edit::value(v);
    }
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

/// Parse curator fields out of a TOML file without needing `AppConfig`.
fn read_curator_from_file(path: &str) -> (bool, String) {
    let content = std::fs::read_to_string(path).expect("read config");
    let val: toml::Value = toml::from_str(&content).expect("parse toml");
    let curator = val.get("curator").expect("curator section");
    let enabled = curator
        .get("enabled")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let cron = curator
        .get("cron")
        .and_then(toml::Value::as_str)
        .unwrap_or("")
        .to_owned();
    (enabled, cron)
}

// ── Test 1: config_update_modifies_curator_fields ─────────────────────────────

#[test]
fn config_update_modifies_curator_fields() {
    // Arrange: write a temp config file with defaults.
    let tmp = write_temp_config(&minimal_config_toml(true, "0 3 * * 0"));
    let path = tmp.path().to_str().unwrap().to_owned();

    // Act: update enabled=false and a new cron expression.
    update_curator_in_file(&path, Some(false), Some("0 0 3 * * *"))
        .expect("update_curator_in_file");

    // Assert: re-parsed file reflects both changes.
    let (enabled, cron) = read_curator_from_file(&path);
    assert!(!enabled, "enabled should be false after update");
    assert_eq!(cron, "0 0 3 * * *", "cron should match the written value");
}

// ── Test 2: shared_config_reflects_curator_update ────────────────────────────

#[tokio::test]
async fn shared_config_reflects_curator_update() {
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // Local struct representing the curator slice of shared config.
    // Avoids pulling the full AppConfig into the lib facade.
    #[derive(Clone)]
    struct CuratorSlice {
        enabled: bool,
        cron: String,
    }

    let shared: Arc<RwLock<CuratorSlice>> = Arc::new(RwLock::new(CuratorSlice {
        enabled: true,
        cron: "0 3 * * 0".to_owned(),
    }));

    // Simulate the PUT handler: write to file, parse, update shared_config.
    let tmp = write_temp_config(&minimal_config_toml(true, "0 3 * * 0"));
    let path = tmp.path().to_str().unwrap().to_owned();

    update_curator_in_file(&path, Some(false), Some("0 0 5 * * *"))
        .expect("update_curator_in_file");
    let (new_enabled, new_cron) = read_curator_from_file(&path);

    // Simulate hot-reload: write parsed values into the shared config.
    {
        let mut guard = shared.write().await;
        guard.enabled = new_enabled;
        guard.cron = new_cron;
    }

    // Concurrent reader must see the updated values, not the original ones.
    let shared_clone = Arc::clone(&shared);
    let (seen_enabled, seen_cron) = tokio::spawn(async move {
        let r = shared_clone.read().await;
        (r.enabled, r.cron.clone())
    })
    .await
    .expect("reader task");

    assert!(!seen_enabled, "concurrent reader must see enabled=false");
    assert_eq!(seen_cron, "0 0 5 * * *", "concurrent reader must see new cron");
}

// ── Tests 3–6: DB layer (require Docker on Linux x86_64) ──────────────────────

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod db_tests {
    use hydeclaw_core::db::curator_runs;
    use sqlx::PgPool;
    use uuid::Uuid;

    // ── Test 3: last_run returns None on empty table ─────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn curator_run_returns_none_on_empty_db(pool: PgPool) {
        // The status endpoint calls last_run before checking — must be None
        // on a freshly-migrated DB with no curator runs.
        let result = curator_runs::last_run(&pool).await.expect("last_run query");
        assert!(
            result.is_none(),
            "last_run must return None on an empty curator_runs table"
        );
    }

    // ── Test 4: insert + read round-trip, then finish ─────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn curator_status_db_fields(pool: PgPool) {
        let before = curator_runs::last_run(&pool)
            .await
            .expect("last_run before insert");
        assert!(before.is_none(), "table must be empty before insert");

        let run_id = curator_runs::insert_run(&pool, "manual")
            .await
            .expect("insert_run");
        assert_ne!(run_id, Uuid::nil(), "insert_run must return a non-nil UUID");

        let run = curator_runs::last_run(&pool)
            .await
            .expect("last_run after insert")
            .expect("run must exist after insert");

        assert_eq!(run.id, run_id, "run id must match the inserted id");
        assert_eq!(run.trigger, "manual", "trigger must be 'manual'");
        assert_eq!(
            run.status, "running",
            "newly inserted run must have status 'running'"
        );
        assert!(
            run.finished_at.is_none(),
            "finished_at must be None for a running run"
        );
        assert!(run.phase1.is_none(), "phase1 must be None before finish");
        assert!(run.phase2.is_none(), "phase2 must be None before finish");
        assert!(run.phase3.is_none(), "phase3 must be None before finish");

        curator_runs::finish_run(
            &pool,
            run_id,
            3,
            5,
            1,
            Some("## Report\n- done"),
            None,
        )
        .await
        .expect("finish_run");

        let finished = curator_runs::last_run(&pool)
            .await
            .expect("last_run after finish")
            .expect("run must still exist after finish");

        assert_eq!(finished.id, run_id);
        assert_eq!(finished.status, "done");
        assert_eq!(finished.phase1, Some(3));
        assert_eq!(finished.phase2, Some(5));
        assert_eq!(finished.phase3, Some(1));
        assert!(
            finished.report_md.is_some(),
            "report_md must be set on success"
        );
        assert!(finished.error.is_none(), "error must be None on success");
        assert!(
            finished.finished_at.is_some(),
            "finished_at must be set after finish"
        );
    }

    // ── Test 5: get_run by id ─────────────────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn get_run_round_trip(pool: PgPool) {
        let run_id = curator_runs::insert_run(&pool, "cron")
            .await
            .expect("insert_run");

        let run = curator_runs::get_run(&pool, run_id)
            .await
            .expect("get_run query")
            .expect("run must exist");

        assert_eq!(run.id, run_id);
        assert_eq!(run.trigger, "cron");

        // Random non-existent id must return None (no panic).
        let missing = curator_runs::get_run(&pool, Uuid::new_v4())
            .await
            .expect("get_run for missing id");
        assert!(
            missing.is_none(),
            "get_run must return None for an unknown id"
        );
    }

    // ── Test 6: list_runs ordering and limit ─────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_runs_ordering_and_limit(pool: PgPool) {
        let id1 = curator_runs::insert_run(&pool, "cron")
            .await
            .expect("insert run 1");
        // Ensure distinct `started_at` timestamps.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let id2 = curator_runs::insert_run(&pool, "manual")
            .await
            .expect("insert run 2");

        let runs = curator_runs::list_runs(&pool, 10)
            .await
            .expect("list_runs");

        assert_eq!(runs.len(), 2, "must return both runs");
        // Newest first: id2 was inserted after id1.
        assert_eq!(runs[0].id, id2, "most recent run must be first");
        assert_eq!(runs[1].id, id1, "older run must be second");

        // Limit is respected.
        let limited = curator_runs::list_runs(&pool, 1)
            .await
            .expect("list_runs with limit 1");
        assert_eq!(limited.len(), 1, "limit=1 must return exactly one run");
        assert_eq!(limited[0].id, id2, "limit=1 must return newest run");
    }
}
