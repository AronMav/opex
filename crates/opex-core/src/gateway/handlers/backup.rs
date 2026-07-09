use anyhow::Context;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde_json::json;
use std::sync::Arc;
use tokio::fs;

use crate::gateway::AppState;
use crate::gateway::clusters::{AgentCore, AuthServices, ConfigServices, InfraServices};
use crate::gateway::restore_stream_core::{
    check_content_length_cap, drain_body_with_cap, DrainError,
};

// Re-use the same file as dto_export::backup_dto so the struct definition has
// a single source of truth (no possible drift between handler shape and TS type).
#[path = "backup_dto_structs.rs"]
mod backup_dto_structs;
pub use backup_dto_structs::BackupEntryDto;

pub(crate) fn routes() -> Router<AppState> {
    // Phase 64 SEC-04: `/api/restore` caps request body size via the per-handler
    // `cfg.config.limits.max_restore_size_mb` (default 500 MB), enforced by the
    // `check_content_length_cap` fast-path AND the `drain_body_with_cap` streaming
    // byte counter. We must `DefaultBodyLimit::disable()` here so axum's default
    // 2 MB extractor limit doesn't short-circuit our own cap check.
    Router::new()
        .route("/api/backup", post(api_create_backup).get(api_list_backups))
        .route("/api/backup/{filename}", get(api_download_backup).delete(api_delete_backup))
        .merge(
            Router::new()
                .route("/api/restore", post(api_restore))
                .layer(DefaultBodyLimit::disable()),
        )
}

const BACKUP_DIR: &str = "backups";

/// Select which postgres container to use from `docker ps` stdout.
///
/// The configured name is **authoritative**: if it appears among the running
/// containers it is always chosen. This prevents a stray container that merely
/// matches the `name=postgres` *substring* filter (e.g. `docker-postgres-test-1`)
/// from being picked over the real one — `docker ps` output order is not
/// deterministic, and the test container has no `opex` role, so picking it
/// makes `pg_dump` fail with `role "opex" does not exist`.
///
/// Auto-discovery only kicks in when the configured container is absent and
/// exactly one postgres container is running (convenience for non-standard
/// deployments). When the configured name is absent and several containers
/// match, we cannot safely guess, so we trust the configured value.
fn select_container<'a>(docker_output: &'a str, configured: &'a str) -> &'a str {
    let running: Vec<&str> = docker_output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if running.contains(&configured) {
        configured
    } else if running.len() == 1 {
        running[0]
    } else {
        configured
    }
}

/// Resolve which postgres container to run `pg_dump`/`pg_restore` against.
/// Lists `docker ps --filter name=postgres` and lets `select_container` apply
/// config precedence; falls back to `configured` if the `docker` call fails.
async fn discover_postgres_container(configured: &str) -> String {
    let out = tokio::process::Command::new("docker")
        .args(["ps", "--filter", "name=postgres", "--format", "{{.Names}}"])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            select_container(&stdout, configured).to_owned()
        }
        _ => configured.to_owned(),
    }
}

/// Discover tables tagged as ephemeral via `COMMENT ON TABLE ... IS '@opex:ephemeral...'`.
/// Used by `run_pg_dump` to build the `--exclude-table` list.
///
/// The tag must be at the start of the comment (anchored LIKE pattern).
async fn ephemeral_tables(container: &str) -> anyhow::Result<Vec<String>> {
    let out = tokio::process::Command::new("docker")
        .args([
            "exec", container, "psql", "-U", "opex", "opex",
            "-tAc",  // tuple-only, unaligned, command — emits one name per line
            "SELECT c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_description d ON d.objoid = c.oid AND d.objsubid = 0 \
             WHERE n.nspname='public' AND c.relkind='r' \
               AND d.description LIKE '@opex:ephemeral%' \
             ORDER BY c.relname",
        ])
        .output().await
        .context("psql ephemeral table discovery failed")?;
    if !out.status.success() {
        anyhow::bail!(
            "ephemeral discovery failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Run pg_dump inside the postgres container and stream output to `dest`.
/// Excluded tables are discovered at runtime via `ephemeral_tables()`,
/// plus a hardcoded `secrets` exclusion (secrets are exported in plaintext
/// for cross-machine portability — see secrets.json in the backup bundle).
async fn run_pg_dump(container: &str, dest: &std::path::Path) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let ephemeral = ephemeral_tables(container).await?;

    let file = tokio::fs::File::create(dest).await
        .with_context(|| format!("create db.dump at {}", dest.display()))?;

    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(["exec", container, "pg_dump", "-U", "opex", "opex", "-Fc"]);
    for table in &ephemeral {
        cmd.args(["--exclude-table", table]);
    }
    // `secrets` is NOT ephemeral — it stores encrypted credentials. We exclude
    // it from the binary dump because secrets are exported separately in
    // plaintext (re-encrypted with the new master key on restore). Hardcoded
    // here so removing the comment from `secrets` cannot accidentally include
    // them in the dump.
    cmd.args(["--exclude-table", "secrets"]);

    cmd.stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("docker exec pg_dump: spawn failed")?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut writer = tokio::io::BufWriter::new(file);
    tokio::io::copy(&mut stdout, &mut writer).await
        .context("streaming pg_dump output to db.dump")?;
    writer.flush().await.context("flush db.dump")?;

    let output = child.wait_with_output().await.context("pg_dump wait")?;
    if !output.status.success() {
        anyhow::bail!("pg_dump failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// Get the PostgreSQL version string from inside the container for the manifest.
async fn get_pg_version(container: &str) -> anyhow::Result<String> {
    let out = tokio::process::Command::new("docker")
        .args(["exec", container, "psql", "-U", "opex", "-t", "-c", "SELECT version()"])
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Copy directory contents to `dst` using `cp -r src/. dst/`.
/// Creates `dst` if it does not exist.
async fn copy_dir_to(src: &str, dst: &std::path::Path) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let src_dot = format!("{src}/.");
    let out = tokio::process::Command::new("cp")
        .args(["-r", &src_dot])
        .arg(dst)
        .output()
        .await
        .with_context(|| format!("cp -r {src}/. failed"))?;
    if !out.status.success() {
        anyhow::bail!("cp failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Remove every entry INSIDE `dir` (keeping `dir` itself). F090: restore uses this
/// to make the filesystem a faithful point-in-time mirror of the backup — files
/// present live but absent from the backup are deleted, matching the DB
/// truncate-replace (a merge would resurrect e.g. an agent config created after
/// the backup). Only called when a rollback snapshot exists, so a mid-restore
/// failure can restore the prior state.
async fn clear_dir_contents(dir: &std::path::Path) -> std::io::Result<()> {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    while let Some(entry) = rd.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            tokio::fs::remove_dir_all(entry.path()).await?;
        } else {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}

/// Run pg_restore inside the postgres container, reading db.dump from stdin.
///
/// Strategy: TRUNCATE all tables in the dump (CASCADE handles FK deps from excluded tables),
/// then pg_restore --data-only. This avoids `--clean` which fails when excluded tables have
/// FK constraints referencing included tables (e.g. cron_runs → scheduled_jobs).
async fn run_pg_restore(container: &str, dump_path: &std::path::Path) -> anyhow::Result<()> {
    // Step 1: get the list of tables present in the dump.
    let tables = list_dump_tables(container, dump_path).await?;

    // F023: TRUNCATE + pg_restore is destructive and NOT transactional across
    // the two docker-exec calls — a pg_restore failure AFTER the truncate
    // (pg-version drift, schema drift, a non-superuser `--disable-triggers`
    // rejection, or a truncated-but-valid dump) would otherwise leave the
    // production DB irreversibly wiped. Take a pg_dump safety snapshot of the
    // CURRENT state before truncating, and roll back to it if the restore fails.
    let safety_path = dump_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("pre-restore-safety.dump");
    pg_dump_snapshot(container, &safety_path)
        .await
        .context("F023 safety snapshot before restore failed — aborting to avoid data loss")?;

    match truncate_and_restore(container, &tables, dump_path).await {
        Ok(()) => {
            let _ = std::fs::remove_file(&safety_path);
            Ok(())
        }
        Err(restore_err) => {
            tracing::error!(
                error = %restore_err,
                "pg_restore failed after TRUNCATE — rolling back to pre-restore snapshot"
            );
            if let Err(rollback_err) = truncate_and_restore(container, &tables, &safety_path).await {
                // Do NOT delete the safety snapshot — it is the only remaining
                // copy of the pre-restore data.
                anyhow::bail!(
                    "RESTORE FAILED AND ROLLBACK FAILED — the DB may be inconsistent. \
                     Pre-restore snapshot preserved at {}. \
                     restore error: {restore_err}; rollback error: {rollback_err}",
                    safety_path.display()
                );
            }
            let _ = std::fs::remove_file(&safety_path);
            anyhow::bail!("pg_restore failed (rolled back to pre-restore state): {restore_err}");
        }
    }
}

/// List the `TABLE DATA public` tables present in a `-Fc` dump (quoted for SQL).
/// True if `name` is a bare PostgreSQL identifier safe to wrap in `"..."` without
/// escaping. Restricts dump-supplied table names to `[A-Za-z0-9_]+` (F087): the
/// names come from `pg_restore --list` on an attacker-uploadable dump and are
/// interpolated into `TRUNCATE "{}" CASCADE`, so anything else could break out of
/// the quoted identifier and inject SQL.
fn is_safe_pg_identifier(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

async fn list_dump_tables(
    container: &str,
    dump_path: &std::path::Path,
) -> anyhow::Result<Vec<String>> {
    let file_list = std::fs::File::open(dump_path)
        .with_context(|| format!("open db.dump for --list: {}", dump_path.display()))?;
    let list_out = tokio::process::Command::new("docker")
        .args(["exec", "-i", container, "pg_restore", "--list"])
        .stdin(std::process::Stdio::from(file_list))
        .output()
        .await
        .context("pg_restore --list failed")?;
    // Lines look like: "234; 0 16442 TABLE DATA public memory_chunks postgres"
    let mut tables = Vec::new();
    for line in String::from_utf8_lossy(&list_out.stdout).lines() {
        if !line.contains(" TABLE DATA public ") {
            continue;
        }
        let Some(name) = line.split_whitespace().nth(6) else {
            continue;
        };
        // F087: reject a crafted dump instead of injecting its table name into SQL.
        if !is_safe_pg_identifier(name) {
            anyhow::bail!(
                "refusing restore: dump lists a non-identifier table name {name:?}"
            );
        }
        tables.push(format!("\"{name}\""));
    }
    Ok(tables)
}

/// Write a `-Fc` pg_dump of the current `opex` DB to `out_path` on the host.
async fn pg_dump_snapshot(container: &str, out_path: &std::path::Path) -> anyhow::Result<()> {
    let out_file = std::fs::File::create(out_path)
        .with_context(|| format!("create safety snapshot file: {}", out_path.display()))?;
    let status = tokio::process::Command::new("docker")
        .args(["exec", container, "pg_dump", "-U", "opex", "-Fc", "opex"])
        .stdout(std::process::Stdio::from(out_file))
        .output()
        .await
        .context("pg_dump safety snapshot spawn failed")?;
    if !status.status.success() {
        anyhow::bail!(
            "pg_dump safety snapshot failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    Ok(())
}

/// TRUNCATE the given tables (CASCADE) then `pg_restore --data-only` from
/// `dump_path`. Shared by the primary restore and the F023 rollback.
async fn truncate_and_restore(
    container: &str,
    tables: &[String],
    dump_path: &std::path::Path,
) -> anyhow::Result<()> {
    // Step 2: TRUNCATE with CASCADE to clear existing rows without dropping schema.
    if !tables.is_empty() {
        let sql = format!("TRUNCATE {} CASCADE", tables.join(", "));
        let trunc = tokio::process::Command::new("docker")
            .args(["exec", container, "psql", "-U", "opex", "opex", "-c", &sql])
            .output()
            .await
            .context("pre-restore TRUNCATE failed")?;
        if !trunc.status.success() {
            anyhow::bail!(
                "pre-restore TRUNCATE failed: {}",
                String::from_utf8_lossy(&trunc.stderr)
            );
        }
    }

    // Step 3: restore data only — schema is already in place from migrations.
    let file = std::fs::File::open(dump_path)
        .with_context(|| format!("open db.dump: {}", dump_path.display()))?;
    let out = tokio::process::Command::new("docker")
        .args([
            "exec", "-i", container,
            "pg_restore", "-U", "opex", "-d", "opex",
            "--data-only",
            // Disable FK triggers during COPY so restore order doesn't cause
            // constraint violations. Requires superuser — opex is POSTGRES_USER.
            "--disable-triggers",
            "-Fc",
        ])
        .stdin(std::process::Stdio::from(file))
        .output()
        .await
        .context("pg_restore spawn failed")?;

    if !out.status.success() {
        anyhow::bail!("pg_restore failed:\n{}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Load agent configs from disk and restart all agents.
async fn restart_agents_from_disk(
    agents: &crate::gateway::clusters::AgentCore,
    infra: &crate::gateway::clusters::InfraServices,
    auth: &crate::gateway::clusters::AuthServices,
    bus: &crate::gateway::clusters::ChannelBus,
    cfg_svc: &crate::gateway::clusters::ConfigServices,
    status: &crate::gateway::clusters::StatusMonitor,
) -> anyhow::Result<Vec<String>> {
    let agent_configs = crate::config::load_agent_configs("config/agents")
        .context("load_agent_configs failed")?;
    let mut restarted = Vec::new();
    for cfg in &agent_configs {
        match super::agents::start_agent_from_config(
            cfg, agents, infra, auth, bus, cfg_svc, status
        ).await {
            Ok((handle, guard)) => {
                let name = cfg.agent.name.clone();
                agents.map.write().await.insert(name.clone(), handle);
                if let Some(g) = guard {
                    auth.access_guards.write().await.insert(name.clone(), g);
                }
                restarted.push(name);
            }
            Err(e) => {
                tracing::error!(agent = %cfg.agent.name, %e, "restart failed after restore");
            }
        }
    }
    Ok(restarted)
}

// ── POST /api/backup ─────────────────────────────────────────────────────────

/// Create a pg_dump-based backup bundle (.tar.gz).
pub(crate) async fn create_backup_internal(
    secrets: &Arc<crate::secrets::SecretsManager>,
    agent_deps: &Arc<tokio::sync::RwLock<crate::gateway::state::AgentDeps>>,
    retention_days: i64,
    postgres_container: &str,
) -> anyhow::Result<String> {
    let now = chrono::Utc::now();
    let filename = format!("opex-{}.tar.gz", now.format("%Y-%m-%d"));

    let container = discover_postgres_container(postgres_container).await;

    // Temp dir with restricted permissions
    let tmpdir = std::env::temp_dir()
        .join(format!("opex-backup-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&tmpdir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmpdir,
            std::fs::Permissions::from_mode(0o700)).await?;
    }

    let result: anyhow::Result<String> = async {
        // 1. pg_dump
        run_pg_dump(&container, &tmpdir.join("db.dump")).await?;

        // 2. Secrets — plaintext, chmod 600
        let backup_secrets = secrets.export_decrypted().await
            .context("secrets export failed")?;
        let secrets_path = tmpdir.join("secrets.json");
        tokio::fs::write(&secrets_path,
            serde_json::to_vec(&backup_secrets).context("secrets serialize")?)
            .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&secrets_path,
                std::fs::Permissions::from_mode(0o600)).await?;
        }

        // 3. Manifest
        let pg_version = get_pg_version(&container).await.unwrap_or_default();
        let mut manifest_excluded = ephemeral_tables(&container).await.unwrap_or_default();
        // Mirror the hardcoded `secrets` exclusion in `run_pg_dump` (see comment there).
        manifest_excluded.push("secrets".to_string());
        tokio::fs::write(
            tmpdir.join("manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 3,
                "created_at": now,
                "pg_version": pg_version,
                "excluded_tables": manifest_excluded,
            })).context("manifest serialize")?,
        ).await?;

        // 4. workspace/ and config/
        let workspace_dir = {
            let deps = agent_deps.read().await;
            deps.workspace_dir.clone()
        };
        copy_dir_to(&workspace_dir, &tmpdir.join("workspace")).await?;
        copy_dir_to("config", &tmpdir.join("config")).await?;

        // 5. tar czf
        tokio::fs::create_dir_all(BACKUP_DIR).await?;
        let filepath = format!("{BACKUP_DIR}/{filename}");
        let tar_out = tokio::process::Command::new("tar")
            .args(["czf", &filepath, "-C"])
            .arg(&tmpdir)
            .arg(".")
            .output().await
            .context("tar czf: spawn failed")?;
        if !tar_out.status.success() {
            anyhow::bail!("tar failed: {}", String::from_utf8_lossy(&tar_out.stderr));
        }

        Ok(filename.clone())
    }.await;

    // Always clean up temp dir, even on error
    let _ = tokio::fs::remove_dir_all(&tmpdir).await;

    let filename = result?;

    // Cleanup old backups
    cleanup_old_backups_v3(now, retention_days).await;

    let size = tokio::fs::metadata(format!("{BACKUP_DIR}/{filename}"))
        .await.map(|m| m.len()).unwrap_or(0);
    tracing::info!(filename = %filename, size_bytes = size, "backup created");
    Ok(filename)
}

async fn cleanup_old_backups_v3(now: chrono::DateTime<chrono::Utc>, retention_days: i64) {
    if retention_days == 0 { return; }  // 0 = disabled
    let cutoff = now - chrono::Duration::days(retention_days);
    let Ok(mut dir) = tokio::fs::read_dir(BACKUP_DIR).await else { return };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        if !name.ends_with(".tar.gz") { continue; }
        if let Some(date_part) = name.strip_prefix("opex-").and_then(|s| s.strip_suffix(".tar.gz"))
            && let Ok(date) = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d")
        {
            let file_dt = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
            if file_dt < cutoff {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }
}

/// Create a backup: pg_dump + workspace + config + secrets, bundle as .tar.gz.
pub(crate) async fn api_create_backup(
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    State(cfg_svc): State<ConfigServices>,
) -> impl IntoResponse {
    let now = chrono::Utc::now();
    let retention = cfg_svc.config.backup.retention_days as i64;
    let container = cfg_svc.config.backup.postgres_container.clone();
    match create_backup_internal(&auth.secrets, &agents.deps, retention, &container).await {
        Ok(filename) => {
            let filepath = format!("{BACKUP_DIR}/{filename}");
            Json(json!({
                "ok": true,
                "filename": filename,
                "path": filepath,
                "created_at": now,
            })).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR,
             Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

// ── GET /api/backup ──────────────────────────────────────────────────────────

pub(crate) async fn api_list_backups() -> impl IntoResponse {
    let mut entries: Vec<BackupEntryDto> = Vec::new();
    if let Ok(mut dir) = fs::read_dir(BACKUP_DIR).await {
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            if !name.ends_with(".tar.gz") { continue; }
            let filename = name.clone();
            if let Ok(meta) = entry.metadata().await {
                let size_bytes = meta.len();
                let created_at = name
                    .strip_prefix("opex-")
                    .and_then(|s| s.strip_suffix(".tar.gz"))
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339())
                    .or_else(|| {
                        meta.modified().ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .and_then(|d| chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0))
                            .map(|dt| dt.to_rfc3339())
                    });
                entries.push(BackupEntryDto { filename, size_bytes, created_at });
            }
        }
    }
    entries.sort_by(|a, b| b.filename.cmp(&a.filename));
    Json(serde_json::json!({ "backups": entries }))
}

// ── GET /api/backup/:filename ─────────────────────────────────────────────────

/// `GET /api/backup/{filename}` — download a backup archive.
///
/// The archive contains plaintext copies of every vault secret, channel
/// credential, and provider API key (see `api_create_backup`). Audit
/// 2026-05-08 flagged that the only barrier was the global Bearer token —
/// matching `/api/restore` we now also require an explicit confirmation
/// header (`X-Confirm-Download: yes-i-am-sure`). Browsers that legitimately
/// download backups must add the header; defence-in-depth against accidental
/// CSRF / log-replay / token-leak fan-out.
pub(crate) async fn api_download_backup(
    headers: axum::http::HeaderMap,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    let confirm = headers
        .get("X-Confirm-Download")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if confirm != "yes-i-am-sure" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "backup download requires header 'X-Confirm-Download: yes-i-am-sure'"
            })),
        )
            .into_response();
    }

    // Prevent path traversal
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") || filename.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid filename"}))).into_response();
    }
    let filepath = format!("{BACKUP_DIR}/{filename}");
    match fs::read(&filepath).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/gzip".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
            ],
            bytes,
        ).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "backup not found"}))).into_response(),
    }
}

// ── DELETE /api/backup/:filename ─────────────────────────────────────────────

pub(crate) async fn api_delete_backup(Path(filename): Path<String>) -> impl IntoResponse {
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") || filename.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid filename"}))).into_response();
    }
    let filepath = format!("{BACKUP_DIR}/{filename}");
    match fs::remove_file(&filepath).await {
        Ok(()) => {
            tracing::info!(filename = %filename, "backup deleted via API");
            Json(json!({"ok": true})).into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "backup not found"}))).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

// ── POST /api/restore ─────────────────────────────────────────────────────────

/// Restore from a v3 `.tar.gz` backup produced by `api_create_backup`.
///
/// The archive must contain:
///   - `manifest.json`  — must have a `"version"` field
///   - `db.dump`        — custom-format pg_dump (restored via pg_restore)
///   - `secrets.json`   — plaintext secrets array
///   - `workspace/`     — workspace directory tree (optional)
///   - `config/`        — agent configs (optional)
#[allow(clippy::too_many_arguments)]
pub(crate) async fn api_restore(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    State(bus): State<crate::gateway::clusters::ChannelBus>,
    State(cfg_svc): State<crate::gateway::clusters::ConfigServices>,
    State(status): State<crate::gateway::clusters::StatusMonitor>,
    req: Request,
) -> axum::response::Response {
    // Guard: require X-Confirm-Restore header
    let headers = req.headers().clone();
    let confirm = headers.get("x-confirm-restore").and_then(|v| v.to_str().ok()).unwrap_or("");
    if confirm != "yes-i-am-sure" {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "restore requires X-Confirm-Restore: yes-i-am-sure header"
        }))).into_response();
    }

    // Size cap
    let cap_mb = cfg_svc.config.limits.max_restore_size_mb;
    let cap_bytes = (cap_mb as usize).saturating_mul(1024 * 1024);
    if let Some((status_code, body)) = check_content_length_cap(&headers, cap_bytes) {
        tracing::warn!(cap_mb, "restore rejected via Content-Length fast-path (payload > cap)");
        return (status_code, [(header::CONTENT_TYPE, "application/json")], body).into_response();
    }
    let body = req.into_body();
    let buf = match drain_body_with_cap(body.into_data_stream(), cap_bytes).await {
        Ok(b) => b,
        Err(DrainError::CapExceeded { observed_bytes, cap_bytes }) => {
            tracing::warn!(observed_bytes, cap_bytes, "restore rejected via streaming byte-counter");
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({
                    "error": "payload exceeds max_restore_size_mb",
                    "cap_bytes": cap_bytes,
                    "observed_bytes": observed_bytes,
                })),
            ).into_response();
        }
        // F115: a transport/IO failure is not an oversize payload — surface the
        // real cause as 400 instead of a self-contradictory 413.
        Err(DrainError::Stream(msg)) => {
            tracing::warn!(error = %msg, "restore body stream failed mid-transfer");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "restore upload failed mid-transfer",
                    "detail": msg,
                })),
            ).into_response();
        }
    };

    // Reject non-tar.gz input: JSON format is fully retired (no backward compatibility)
    if buf.len() < 2 || buf[0] != 0x1f || buf[1] != 0x8b {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "unsupported format: only .tar.gz backups (v3) are accepted"
        }))).into_response();
    }

    let container = discover_postgres_container(
        &cfg_svc.config.backup.postgres_container
    ).await;

    // Write .tar.gz to temp file
    let tmpdir = std::env::temp_dir()
        .join(format!("opex-restore-{}", uuid::Uuid::new_v4()));
    if let Err(e) = tokio::fs::create_dir_all(&tmpdir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("tmpdir: {e}")}))).into_response();
    }
    let tar_path = tmpdir.join("restore.tar.gz");
    if let Err(e) = tokio::fs::write(&tar_path, &buf).await {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("write tar: {e}")}))).into_response();
    }

    // Extract tar
    let extract_dir = tmpdir.join("extracted");
    if let Err(e) = tokio::fs::create_dir_all(&extract_dir).await {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("mkdir extracted: {e}")}))).into_response();
    }
    let tar_out = tokio::process::Command::new("tar")
        .args(["xzf"])
        .arg(&tar_path)
        .arg("-C")
        .arg(&extract_dir)
        .output().await;
    match tar_out {
        Ok(o) if !o.status.success() => {
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": format!("tar extract failed: {}", String::from_utf8_lossy(&o.stderr))
            }))).into_response();
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("tar spawn: {e}")}))).into_response();
        }
        Ok(_) => {}
    }

    // Validate manifest
    let manifest_path = extract_dir.join("manifest.json");
    let manifest: serde_json::Value = match tokio::fs::read(&manifest_path).await {
        Ok(b) => match serde_json::from_slice(&b) {
            Ok(v) => v,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&tmpdir).await;
                return (StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("manifest.json invalid: {e}")}))).into_response();
            }
        },
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("manifest.json missing: {e}")}))).into_response();
        }
    };
    if manifest.get("version").is_none() {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::BAD_REQUEST,
            Json(json!({"error": "unsupported backup format: missing version"}))).into_response();
    }

    // Pre-check: db.dump must exist before we disrupt anything
    let dump_path = extract_dir.join("db.dump");
    if !dump_path.exists() {
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "backup archive is missing db.dump"
        }))).into_response();
    }

    // Snapshot workspace for rollback
    let workspace_bak = tmpdir.join("workspace.bak.tar.gz");
    let workspace_bak_ok = tokio::process::Command::new("tar")
        .args(["czf"])
        .arg(&workspace_bak)
        .args(["-C", ".", "workspace"])
        .output().await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !workspace_bak_ok {
        tracing::warn!("workspace snapshot for rollback failed — rollback will not be available if restore fails");
    }

    // Snapshot config for rollback too (F091): config restore is as critical as
    // workspace — a failed copy must be rolled back and surfaced, not silently
    // reported as success.
    let config_bak = tmpdir.join("config.bak.tar.gz");
    let config_bak_ok = tokio::process::Command::new("tar")
        .args(["czf"])
        .arg(&config_bak)
        .args(["-C", ".", "config"])
        .output().await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !config_bak_ok {
        tracing::warn!("config snapshot for rollback failed — rollback will not be available if restore fails");
    }

    tracing::warn!("RESTORE initiated from pg_dump backup (v3)");

    // Stop all running agents
    {
        let mut map = agents.map.write().await;
        let names: Vec<_> = map.keys().cloned().collect();
        for name in &names {
            if let Some(h) = map.remove(name) {
                h.shutdown(&agents.scheduler).await;
                tracing::info!(agent = %name, "agent stopped for restore");
            }
        }
    }

    // pg_restore
    if let Err(e) = run_pg_restore(&container, &dump_path).await {
        tracing::error!("pg_restore failed: {e}");
        let restarted = restart_agents_from_disk(&agents, &infra, &auth, &bus, &cfg_svc, &status).await
            .unwrap_or_default();
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
            "error": format!("pg_restore failed: {e}"),
            "agents_restarted_from_old_state": restarted,
        }))).into_response();
    }

    // Restore secrets
    let secrets_bytes = tokio::fs::read(extract_dir.join("secrets.json")).await
        .unwrap_or_default();
    let plaintext_secrets: Vec<crate::secrets::PlaintextSecret> =
        serde_json::from_slice(&secrets_bytes).unwrap_or_default();
    let secret_count = plaintext_secrets.len();
    if let Err(e) = auth.secrets.restore_plaintext(plaintext_secrets).await {
        let restarted = restart_agents_from_disk(&agents, &infra, &auth, &bus, &cfg_svc, &status).await
            .unwrap_or_default();
        let _ = tokio::fs::remove_dir_all(&tmpdir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("secrets restore failed: {e}"),
                "agents_restarted": restarted,
            }))).into_response();
    }

    // Restore workspace and config
    let workspace_src = extract_dir.join("workspace");
    if workspace_src.exists() {
        // F090: faithful restore — clear the live dir so files absent from the
        // backup are removed (not merged). Only when the rollback snapshot exists,
        // so a mid-restore failure can restore the prior state; else fall back to
        // merge rather than clearing with no safety net.
        if workspace_bak_ok
            && let Err(e) = clear_dir_contents(std::path::Path::new("workspace")).await
        {
            tracing::warn!(error = %e, "workspace clear-before-restore failed — copying as merge");
        }
        let workspace_src_str = workspace_src.to_string_lossy().into_owned();
        if let Err(e) = copy_dir_to(&workspace_src_str, std::path::Path::new("workspace")).await {
            // Rollback workspace from snapshot (only if snapshot was created successfully)
            if workspace_bak_ok {
                let _ = tokio::process::Command::new("tar")
                    .args(["xzf"]).arg(&workspace_bak).args(["-C", "."]).output().await;
            } else {
                tracing::error!("workspace rollback skipped: snapshot was not created");
            }
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("workspace restore failed: {e}")}))).into_response();
        }
    }
    let config_src = extract_dir.join("config");
    if config_src.exists() {
        // F090: faithful restore — clear config/ so a stale agent TOML (e.g. an
        // agent created after the backup) doesn't survive and get restarted from
        // disk with no DB backing. Gated on the snapshot for rollback safety.
        if config_bak_ok
            && let Err(e) = clear_dir_contents(std::path::Path::new("config")).await
        {
            tracing::warn!(error = %e, "config clear-before-restore failed — copying as merge");
        }
        let config_src_str = config_src.to_string_lossy().into_owned();
        // F091: check the config copy (was `let _ = ...`). A failed copy left the
        // system half-restored yet returned {"ok":true}; mirror the workspace path
        // — roll back from the snapshot and return 500.
        if let Err(e) = copy_dir_to(&config_src_str, std::path::Path::new("config")).await {
            if config_bak_ok {
                let _ = tokio::process::Command::new("tar")
                    .args(["xzf"]).arg(&config_bak).args(["-C", "."]).output().await;
            } else {
                tracing::error!("config rollback skipped: snapshot was not created");
            }
            let _ = tokio::fs::remove_dir_all(&tmpdir).await;
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("config restore failed: {e}")}))).into_response();
        }
    }

    // Mark setup complete
    let _ = opex_db::sys_flags::upsert(&infra.db, "setup_complete", json!(true))
        .await
        .inspect_err(|e| tracing::warn!(%e, "restore: set setup_complete failed"));

    // Restart agents from restored configs
    let restarted = restart_agents_from_disk(
        &agents, &infra, &auth, &bus, &cfg_svc, &status
    ).await.unwrap_or_default();

    let _ = tokio::fs::remove_dir_all(&tmpdir).await;
    tracing::warn!(agents = ?restarted, "RESTORE complete (pg_dump v3)");

    Json(json!({
        "ok": true,
        "restored": {
            "db": "pg_restore ok",
            "secrets": secret_count,
        },
        "restarted_agents": restarted,
    })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clear_dir_contents_removes_all_but_keeps_dir() {
        // F090: verify the destructive helper clears files AND nested dirs, keeps
        // the target dir itself, and is a no-op on a missing dir (never panics /
        // over-deletes the parent).
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("workspace");
        std::fs::create_dir_all(dir.join("agents/Aria")).unwrap();
        std::fs::write(dir.join("MEMORY.md"), b"x").unwrap();
        std::fs::write(dir.join("agents/Aria/note.md"), b"y").unwrap();

        clear_dir_contents(&dir).await.unwrap();

        assert!(dir.exists(), "target dir itself must survive");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "dir must be empty");

        // Missing dir → Ok (no-op), does not touch the parent.
        let missing = root.path().join("does-not-exist");
        clear_dir_contents(&missing).await.unwrap();
        assert!(root.path().exists());
    }

    #[tokio::test]
    async fn faithful_restore_removes_files_absent_from_backup() {
        // F090 E2E (filesystem semantics): the exact scenario — an agent config
        // created AFTER the backup must NOT survive a restore of that older
        // backup. Reproduces the handler's clear→copy sequence on absolute paths
        // (no CWD dependency) using the same private helpers the handler calls.
        let root = tempfile::tempdir().unwrap();

        // Live runtime config: Old + New (New created after the backup was taken).
        let live = root.path().join("config");
        std::fs::create_dir_all(live.join("agents")).unwrap();
        std::fs::write(live.join("agents/Old.toml"), b"old-live").unwrap();
        std::fs::write(live.join("agents/New.toml"), b"new-live").unwrap();

        // The backup's config dir contains ONLY Old.
        let backup = root.path().join("backup/config");
        std::fs::create_dir_all(backup.join("agents")).unwrap();
        std::fs::write(backup.join("agents/Old.toml"), b"old-backup").unwrap();

        // Faithful restore = clear the live dir, then copy the backup in.
        clear_dir_contents(&live).await.unwrap();
        copy_dir_to(&backup.to_string_lossy(), &live).await.unwrap();

        assert!(
            !live.join("agents/New.toml").exists(),
            "an agent created after the backup must be REMOVED (faithful restore, not merge)"
        );
        assert_eq!(
            std::fs::read(live.join("agents/Old.toml")).unwrap(),
            b"old-backup",
            "Old must be restored from the backup copy"
        );
    }

    #[tokio::test]
    async fn faithful_restore_rolls_back_on_copy_failure() {
        // F090: if the copy fails AFTER the dir was cleared, the snapshot rollback
        // must restore the prior state (so a mid-restore failure is not data loss).
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("workspace");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("keep.md"), b"live-data").unwrap();

        // Snapshot the live dir contents (as the handler does before clearing).
        let snap = root.path().join("workspace.bak.tar.gz");
        let snap_ok = tokio::process::Command::new("tar")
            .args(["czf"]).arg(&snap).args(["-C"]).arg(&live).arg(".")
            .output().await.map(|o| o.status.success()).unwrap_or(false);
        assert!(snap_ok, "snapshot must be created");

        // Clear, then a copy that FAILS (source does not exist).
        clear_dir_contents(&live).await.unwrap();
        let copy_res = copy_dir_to(
            &root.path().join("missing-src").to_string_lossy(),
            &live,
        ).await;
        assert!(copy_res.is_err(), "copy from a missing source must fail");
        assert!(!live.join("keep.md").exists(), "dir is empty after clear+failed copy");

        // Rollback: clear + extract the snapshot back into the dir.
        clear_dir_contents(&live).await.unwrap();
        let _ = tokio::process::Command::new("tar")
            .args(["xzf"]).arg(&snap).args(["-C"]).arg(&live)
            .output().await;
        assert!(
            live.join("keep.md").exists(),
            "rollback must restore the pre-restore snapshot"
        );
        assert_eq!(std::fs::read(live.join("keep.md")).unwrap(), b"live-data");
    }

    #[test]
    fn safe_pg_identifier_rejects_injection() {
        // F087: legit snake_case names pass; anything with quotes/spaces/parens
        // (the SQL-injection primitives) is rejected.
        assert!(is_safe_pg_identifier("memory_chunks"));
        assert!(is_safe_pg_identifier("scheduled_jobs"));
        assert!(is_safe_pg_identifier("Table1"));
        assert!(!is_safe_pg_identifier(""));
        assert!(!is_safe_pg_identifier("t\"; COPY x TO PROGRAM 'sh'--"));
        assert!(!is_safe_pg_identifier("a\"b"));
        assert!(!is_safe_pg_identifier("a b"));
        assert!(!is_safe_pg_identifier("a-b"));
        assert!(!is_safe_pg_identifier("a.b"));
    }

    // Regression: `docker ps --filter name=postgres` is a *substring* filter, so
    // it also matches `docker-postgres-test-1`. Docker's output order is not
    // deterministic — when the test container is listed first, the old "first
    // line wins" logic picked it, and pg_dump failed with `role "opex" does
    // not exist` (the test container has no opex role). The configured
    // container must win whenever it is present.
    #[test]
    fn select_container_prefers_configured_over_substring_match() {
        assert_eq!(
            select_container("docker-postgres-test-1\ndocker-postgres-1\n", "docker-postgres-1"),
            "docker-postgres-1"
        );
    }

    // When the configured container is absent and several postgres containers
    // are running, we cannot safely guess — trust the configured value rather
    // than picking an arbitrary first line.
    #[test]
    fn select_container_trusts_config_when_ambiguous() {
        assert_eq!(
            select_container("docker-postgres-1\ndocker-postgres-2\n", "configured-pg"),
            "configured-pg"
        );
    }

    // Auto-discovery convenience: exactly one postgres container running under a
    // non-standard name → use it even though it differs from the configured one.
    #[test]
    fn select_container_autodiscovers_single_nonstandard() {
        assert_eq!(
            select_container("my-pg-1\n", "docker-postgres-1"),
            "my-pg-1"
        );
    }

    #[test]
    fn select_container_falls_back_when_output_empty() {
        assert_eq!(select_container("", "docker-postgres-1"), "docker-postgres-1");
    }

    #[test]
    fn select_container_trims_whitespace() {
        assert_eq!(select_container("  my-pg-1  \n", "fb"), "my-pg-1");
    }

    /// `ephemeral_tables()` parses psql -tAc output (one name per line, with
    /// possible trailing whitespace). This test mirrors the parsing logic
    /// (the actual function requires docker to test end-to-end).
    #[test]
    fn parse_ephemeral_lines_strips_whitespace_and_empties() {
        fn parse(output: &str) -> Vec<String> {
            output.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
        }
        let raw = "messages\nsessions\n\n  outbound_queue  \n\n";
        let got = parse(raw);
        assert_eq!(got, vec!["messages", "sessions", "outbound_queue"]);
    }
}
