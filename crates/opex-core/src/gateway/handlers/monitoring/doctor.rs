//! `/api/doctor` — composite health check (16 sub-checks: database,
//! toolgate, browser-renderer, secrets, channels, agents,
//! tool-health, migrations, pgvector, memory-worker, providers,
//! security-audit, network, backup, disk, agent-table-classification).
//!
//! Two heavyweight subchecks live here as private helpers:
//! - [`check_provider_reachability`] — probes every enabled
//!   provider's `/v1/models` endpoint with the SSRF-guarded HTTP
//!   client.
//! - [`check_security_audit`] — scans `workspace/` for credential
//!   regex matches and inspects per-agent `tools.deny` lists.

use axum::{
    extract::State,
    response::Json,
};
use serde_json::{Value, json};

use super::{CheckResult, CheckStatus};
use crate::gateway::clusters::{AgentCore, AuthServices, ConfigServices, InfraServices, StatusMonitor};

// ── Provider reachability check ───────────────────────────────────────────────

/// Whether a provider's reachability probe should use the direct (non-SSRF)
/// HTTP client. Provider `base_url`s are admin-configured (Providers page) and
/// trusted; the SSRF resolver blocks loopback HOSTNAMES (`localhost` →
/// 127.0.0.1), false-flagging healthy local providers (e.g. a self-hosted TTS on
/// `http://localhost:8088`). This operator-only probe reads only the status code,
/// so a direct client for local endpoints carries no exfiltration risk. External
/// endpoints keep the SSRF-guarded client (Phase 64 SEC-01). Private IP-literal
/// hosts (e.g. `http://10.0.0.5:8000`) keep the SSRF client too — they already
/// bypass the resolver since an IP literal needs no DNS resolution.
fn probe_uses_direct_client(base_url: &str) -> bool {
    base_url.starts_with("http://localhost") || base_url.starts_with("http://127.")
}

/// Whether the probe's HTTP status means the provider is reachable. ANY response
/// proves reachability; 2xx / 401 / 403 / 404 / 405 are acceptable (external APIs
/// and non-OpenAI local providers legitimately answer 404/405 to `/v1/models`).
/// Other statuses (4xx/5xx) mean the server is up but unhealthy → caller warns.
fn probe_status_reachable(status: u16) -> bool {
    (200..300).contains(&status) || matches!(status, 401 | 403 | 404 | 405)
}

async fn check_provider_reachability(infra: &InfraServices, auth: &AuthServices) -> CheckResult {
    let start = std::time::Instant::now();
    let providers = match crate::db::providers::list_providers(&infra.db).await {
        Ok(p) => p,
        Err(e) => return CheckResult::error(
            format!("failed to list providers: {e}"),
            start.elapsed().as_millis() as u64,
            Some("check database connectivity".into()),
        ),
    };

    let enabled: Vec<_> = providers.into_iter().filter(|p| p.enabled).collect();
    if enabled.is_empty() {
        return CheckResult {
            status: CheckStatus::Ok,
            message: "no providers configured".into(),
            latency_ms: Some(start.elapsed().as_millis() as u64),
            fix_hint: Some("add a provider in the Providers page".into()),
            details: None,
        };
    }

    // External providers go through the SSRF-guarded client (SEC-01). Local,
    // admin-configured providers (loopback hostnames) use a direct client so the
    // SSRF resolver does not false-flag a healthy `http://localhost:…` provider.
    let http = crate::net::ssrf::ssrf_http_client(std::time::Duration::from_secs(3));
    let direct_http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut results = serde_json::Map::new();
    let mut any_error = false;
    let mut any_warn = false;

    for p in &enabled {
        let provider_start = std::time::Instant::now();

        let base_url = p.base_url.as_deref().unwrap_or("");
        let is_local = base_url.starts_with("http://localhost") || base_url.starts_with("http://127.");

        let has_cred = is_local || auth.secrets.get_scoped(
            crate::agent::providers::PROVIDER_CREDENTIALS,
            &p.id.to_string(),
        ).await.is_some();

        let (status, message, fix_hint) = if base_url.is_empty() {
            any_warn = true;
            ("warn", format!("{} has no base_url configured", p.name),
             Some("set base_url in Providers page".to_string()))
        } else if !has_cred {
            any_warn = true;
            ("warn", format!("{} has no API credential stored", p.name),
             Some("add API key in Providers page".to_string()))
        } else {
            let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
            let client = if probe_uses_direct_client(base_url) { &direct_http } else { &http };
            match client.get(&url).send().await {
                Ok(r) if probe_status_reachable(r.status().as_u16()) => {
                    // Any response proves reachability; non-OpenAI providers (e.g. a
                    // local TTS) legitimately answer 404/405 to GET /v1/models.
                    ("ok", format!("{} reachable", p.name), None)
                }
                Ok(r) => {
                    any_warn = true;
                    ("warn", format!("{} returned HTTP {}", p.name, r.status()),
                     Some("check provider base_url in Providers page".to_string()))
                }
                Err(_) => {
                    any_error = true;
                    ("error", format!("{} unreachable", p.name),
                     Some("check provider base_url and network connectivity".to_string()))
                }
            }
        };

        let ms = provider_start.elapsed().as_millis() as u64;
        results.insert(p.name.clone(), serde_json::json!({
            "status": status,
            "message": message,
            "latency_ms": ms,
            "fix_hint": fix_hint,
            "category": p.category,
        }));
    }

    let overall_status = if any_error { CheckStatus::Error }
        else if any_warn { CheckStatus::Warn }
        else { CheckStatus::Ok };
    let ok_count = results.values()
        .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("ok"))
        .count();

    CheckResult {
        status: overall_status,
        message: format!("{}/{} providers reachable", ok_count, enabled.len()),
        latency_ms: Some(start.elapsed().as_millis() as u64),
        fix_hint: None,
        details: Some(serde_json::Value::Object(results)),
    }
}

// ── Security audit check ─────────────────────────────────────────────────────

async fn check_security_audit(_infra: &InfraServices) -> CheckResult {
    use regex::Regex;

    let start = std::time::Instant::now();

    // Credential patterns
    let patterns: &[(&'static str, &'static str)] = &[
        (r"sk-[a-zA-Z0-9]{40,}", "OpenAI key"),
        (r"ghp_[a-zA-Z0-9]{36}", "GitHub token"),
        (r"AIza[0-9A-Za-z\-_]{35}", "Google API key"),
        (r#"[Aa][Pp][Ii][_-]?[Kk][Ee][Yy]\s*[:=]\s*['"]?[a-zA-Z0-9]{20,}"#, "generic API key"),
    ];

    // Walk workspace/ (skip uploads/)
    let workspace_dir = std::path::Path::new("workspace");
    let mut credential_findings: Vec<serde_json::Value> = Vec::new();
    let mut files_scanned = 0usize;

    fn walk_dir_sync(
        dir: &std::path::Path,
        compiled: &[(regex::Regex, &str)],
        findings: &mut Vec<serde_json::Value>,
        count: &mut usize,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "uploads") { continue; }
                if *count >= 1000 { break; }
                walk_dir_sync(&path, compiled, findings, count);
            } else {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !["md", "yaml", "yml", "txt"].contains(&ext) { continue; }
                *count += 1;
                if *count > 1000 { break; }
                let Ok(content) = std::fs::read(&path) else { continue };
                if content.len() > 100_000 { continue; }
                let text = String::from_utf8_lossy(&content);
                for (re, pattern_name) in compiled {
                    if re.is_match(&text) {
                        findings.push(serde_json::json!({
                            "file": path.display().to_string(),
                            "pattern": pattern_name,
                        }));
                        break; // one finding per file
                    }
                }
            }
        }
    }

    // Run the blocking filesystem walk off the async thread to avoid blocking the executor
    if workspace_dir.exists() {
        let workspace_dir_owned = workspace_dir.to_path_buf();
        let compiled_owned: Vec<(Regex, &'static str)> = patterns.iter()
            .filter_map(|(pat, name)| Regex::new(pat).ok().map(|r| (r, *name)))
            .collect();
        let (findings, scanned) = tokio::task::spawn_blocking(move || {
            let mut findings: Vec<serde_json::Value> = Vec::new();
            let mut count = 0usize;
            walk_dir_sync(&workspace_dir_owned, &compiled_owned, &mut findings, &mut count);
            (findings, count)
        })
        .await
        .unwrap_or_default();
        credential_findings = findings;
        files_scanned = scanned;
    }

    // Tool deny-list audit
    let config_dir = std::path::Path::new("config/agents");
    let mut deny_list_issues: Vec<serde_json::Value> = Vec::new();
    let dangerous_tools = ["code_exec", "process"];

    if let Ok(entries) = std::fs::read_dir(config_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") { continue; }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(val) = toml::from_str::<toml::Value>(&text) else { continue };

            let is_base = val.get("agent")
                .and_then(|a| a.get("base"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            if is_base { continue; } // base agents are intentionally unrestricted

            let deny_list = val.get("agent")
                .and_then(|a| a.get("tools"))
                .and_then(|t| t.get("deny"))
                .and_then(|d| d.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                .unwrap_or_default();

            let agent_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");

            for tool in &dangerous_tools {
                if !deny_list.contains(tool) {
                    deny_list_issues.push(serde_json::json!({
                        "agent": agent_name,
                        "tool": tool,
                        "issue": "dangerous tool not in deny list",
                    }));
                }
            }
        }
    }

    // Suppress unused field warning — _infra is passed for future extensibility

    let ms = start.elapsed().as_millis() as u64;
    let has_cred_leaks = !credential_findings.is_empty();
    let has_deny_issues = !deny_list_issues.is_empty();

    let status = if has_cred_leaks {
        CheckStatus::Error
    } else if has_deny_issues {
        CheckStatus::Warn
    } else {
        CheckStatus::Ok
    };

    let message = match (has_cred_leaks, has_deny_issues) {
        (true, _) => format!("{} credential leak(s) found in workspace files", credential_findings.len()),
        (false, true) => format!("{} agent(s) missing tool deny-list entries", deny_list_issues.len()),
        (false, false) => format!("no issues found ({files_scanned} files scanned)"),
    };

    CheckResult {
        status,
        message,
        latency_ms: Some(ms),
        fix_hint: if has_cred_leaks {
            Some("move API keys to secrets vault via Secrets page; remove from workspace files".into())
        } else if has_deny_issues {
            Some("add dangerous tools to deny list in agent config".into())
        } else {
            None
        },
        details: Some(serde_json::json!({
            "files_scanned": files_scanned,
            "credential_findings": credential_findings,
            "deny_list_issues": deny_list_issues,
        })),
    }
}

// ── Doctor / Health-check API ──

pub(crate) async fn api_doctor(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
) -> Json<Value> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let config = cfg_svc.shared_config.read().await;
    let toolgate_url = config.toolgate_url.clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    let br_base = std::env::var("BROWSER_RENDERER_URL")
        .unwrap_or_else(|_| "http://localhost:9020".to_string());
    drop(config);

    // ── 1. Database check ──────────────────────────────────────────────────
    let db_clone = infra.db.clone();
    let database_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let ok = sqlx::query("SELECT 1").execute(&db_clone).await.is_ok();
            let ms = start.elapsed().as_millis() as u64;
            if ok {
                CheckResult::ok("database reachable", ms)
            } else {
                CheckResult::error(
                    "database unreachable",
                    ms,
                    Some("check DATABASE_URL and PostgreSQL service".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("database"));

    // ── 2. Toolgate check ─────────────────────────────────────────────────
    let tg_http = http.clone();
    let tg_url = toolgate_url.clone();
    let toolgate_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let result = tg_http.get(format!("{tg_url}/health")).send().await;
            let ms = start.elapsed().as_millis() as u64;
            match result {
                Ok(r) if r.status().is_success() => {
                    let body: Value = r.json().await.unwrap_or(Value::Null);
                    let providers = body.get("active_providers").cloned().unwrap_or(Value::Null);
                    let mut cr = CheckResult::ok("toolgate reachable", ms);
                    cr.details = Some(json!({"providers": providers}));
                    cr
                }
                _ => CheckResult::error(
                    "toolgate unreachable",
                    ms,
                    Some("check toolgate process is running".into()),
                ),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("toolgate"));

    // ── 3. Browser renderer check ─────────────────────────────────────────
    let br_http = http.clone();
    let br_url = br_base.clone();
    let browser_renderer_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let ok = br_http.get(format!("{br_url}/health")).send().await
                .map(|r| r.status().is_success()).unwrap_or(false);
            let ms = start.elapsed().as_millis() as u64;
            if ok {
                CheckResult::ok("browser renderer reachable", ms)
            } else {
                CheckResult::warn(
                    "browser renderer not reachable",
                    ms,
                    Some("start browser-renderer container if screenshot tools are needed".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("browser_renderer"));

    // ── 5. Secrets check ──────────────────────────────────────────────────
    let mut missing_critical: Vec<String> = Vec::new();
    if let Ok(providers) = crate::db::providers::list_providers_by_type(&infra.db, "text").await {
        for p in &providers {
            let has_key = auth.secrets.get_scoped(
                crate::agent::providers::PROVIDER_CREDENTIALS,
                &p.id.to_string(),
            ).await.is_some();
            if !has_key {
                missing_critical.push(format!("LLM:{}", p.name));
            }
        }
    }
    if let Ok(channels) = sqlx::query_as::<_, (sqlx::types::Uuid, String, String)>(
        "SELECT id, agent_name, channel_type FROM agent_channels WHERE status != 'deleted'"
    ).fetch_all(&infra.db).await {
        for (id, agent, ch_type) in &channels {
            if auth.secrets.get_scoped("CHANNEL_CREDENTIALS", &id.to_string()).await.is_none() {
                missing_critical.push(format!("Channel:{agent}:{ch_type}"));
            }
        }
    }
    let secrets_count = auth.secrets.list().await.map(|v| v.len()).unwrap_or(0);
    let secrets_check = {
        let mut cr = if missing_critical.is_empty() {
            CheckResult::ok(format!("{secrets_count} secrets configured"), 0)
        } else {
            CheckResult::warn(
                format!("{} missing credential(s)", missing_critical.len()),
                0,
                Some("add missing credentials via the Secrets page or vault API".into()),
            )
        };
        cr.details = Some(json!({
            "count": secrets_count,
            "missing_critical": missing_critical,
        }));
        cr
    };

    // ── 6. Channels health check ───────────────────────────────────────────
    let ch_http = http.clone();
    let channels_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let ok = ch_http.get("http://localhost:3100/health").send().await
                .map(|r| r.status().is_success()).unwrap_or(false);
            let ms = start.elapsed().as_millis() as u64;
            if ok {
                CheckResult::ok("channels adapter reachable", ms)
            } else {
                CheckResult::warn(
                    "channels adapter not reachable",
                    ms,
                    Some("check channels process is running".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("channels"));

    // ── 7. Agent statuses ─────────────────────────────────────────────────
    let agents_map = agents.map.read().await;
    let agent_count = agents_map.len();
    let mut agents_details = serde_json::Map::new();
    for (name, _handle) in agents_map.iter() {
        agents_details.insert(name.clone(), json!({"status": "ok"}));
    }
    drop(agents_map);
    let agents_check = {
        let mut cr = CheckResult::ok(format!("{agent_count} agent(s) loaded"), 0);
        cr.details = Some(Value::Object(agents_details));
        cr
    };

    // ── 8. Tool health ────────────────────────────────────────────────────
    let degraded_tools = crate::db::tool_quality::get_degraded_tools(&infra.db)
        .await.unwrap_or_default();
    let degraded_count = degraded_tools.len();
    let tool_health_check = {
        let mut cr = if degraded_count == 0 {
            CheckResult::ok("all tools healthy", 0)
        } else {
            CheckResult::warn(
                format!("{degraded_count} degraded tool(s)"),
                0,
                Some("review tool audit log for repeated failures".into()),
            )
        };
        cr.details = Some(json!({
            "degraded": degraded_tools,
            "degraded_count": degraded_count,
        }));
        cr
    };

    // ── 9. DB migration lag check ─────────────────────────────────────────
    let mig_db = infra.db.clone();
    let migrations_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            // applied: rows in sqlx tracking table
            let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
                .fetch_one(&mig_db)
                .await
                .unwrap_or(0);
            // total: load migration files from disk at runtime (same path used by main.rs)
            let total = match sqlx::migrate::Migrator::new(std::path::Path::new("migrations")).await {
                Ok(m) => m.migrations.len() as i64,
                Err(_) => applied, // can't determine total — assume up to date
            };
            let pending = (total - applied).max(0);
            let ms = start.elapsed().as_millis() as u64;
            if pending > 0 {
                CheckResult::warn(
                    format!("{pending} migration(s) pending"),
                    ms,
                    Some("restart the service to apply pending migrations".into()),
                )
            } else {
                CheckResult::ok(format!("all {total} migrations applied"), ms)
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("migrations"));

    // ── 10. pgvector extension check ──────────────────────────────────────
    let pg_db = infra.db.clone();
    let pgvector_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let present: bool = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'vector')",
            )
            .fetch_one(&pg_db)
            .await
            .unwrap_or(false);
            let ms = start.elapsed().as_millis() as u64;
            if present {
                CheckResult::ok("pgvector extension installed", ms)
            } else {
                CheckResult::error(
                    "pgvector extension missing",
                    ms,
                    Some("run: CREATE EXTENSION IF NOT EXISTS vector; in your database".into()),
                )
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("pgvector"));

    // ── 11. Memory worker check (Linux only) ──────────────────────────────
    #[cfg(target_os = "linux")]
    let memory_worker_check = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            let start = std::time::Instant::now();
            let out = tokio::process::Command::new("systemctl")
                .args(["--user", "is-active", "opex-memory-worker"])
                .output()
                .await;
            let ms = start.elapsed().as_millis() as u64;
            match out {
                Ok(o) if o.status.success() => CheckResult::ok("memory worker active", ms),
                Ok(_) => CheckResult::warn(
                    "memory worker not active",
                    ms,
                    Some("start with: systemctl --user start opex-memory-worker".into()),
                ),
                Err(e) => CheckResult::warn(
                    format!("memory worker check failed: {}", e),
                    ms,
                    None,
                ),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("memory_worker"));

    #[cfg(not(target_os = "linux"))]
    let memory_worker_check = CheckResult {
        status: CheckStatus::Ok,
        message: "memory worker check not available on this platform".into(),
        latency_ms: None,
        fix_hint: None,
        details: None,
    };

    // ── 12. Provider reachability check ──────────────────────────────────
    let (providers_check, security_check) = tokio::join!(
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            check_provider_reachability(&infra, &auth),
        ),
        tokio::time::timeout(
            std::time::Duration::from_secs(12),
            check_security_audit(&infra),
        ),
    );
    let providers_check = providers_check.unwrap_or_else(|_| CheckResult::timeout("providers"));
    let security_check = security_check.unwrap_or_else(|_| CheckResult::timeout("security_audit"));

    // ── 13. Disk space check (Linux only) ─────────────────────────────────
    #[cfg(target_os = "linux")]
    let disk_check = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            let start = std::time::Instant::now();
            let out = tokio::process::Command::new("df")
                .args(["-k", "--output=avail", "."])
                .output()
                .await;
            let ms = start.elapsed().as_millis() as u64;
            match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let avail_kb: i64 = text
                        .lines()
                        .nth(1)
                        .and_then(|l| l.trim().parse().ok())
                        .unwrap_or(i64::MAX);
                    let avail_mb = avail_kb / 1024;
                    if avail_kb < 102_400 {
                        CheckResult::error(
                            format!("{} MB disk free (critical)", avail_mb),
                            ms,
                            Some("free disk space immediately — system may become unstable".into()),
                        )
                    } else if avail_kb < 512_000 {
                        CheckResult::warn(
                            format!("{} MB disk free (low)", avail_mb),
                            ms,
                            Some("consider freeing disk space or expanding storage".into()),
                        )
                    } else {
                        CheckResult::ok(format!("{} MB disk free", avail_mb), ms)
                    }
                }
                Ok(o) => CheckResult::warn(
                    format!(
                        "df exited with error: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                    ms,
                    None,
                ),
                Err(e) => CheckResult::warn(format!("disk check failed: {}", e), ms, None),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("disk"));

    #[cfg(not(target_os = "linux"))]
    let disk_check = CheckResult {
        status: CheckStatus::Ok,
        message: "disk check not available on this platform".into(),
        latency_ms: None,
        fix_hint: None,
        details: None,
    };

    // ── 16. Agent table classification check ──────────────────────────────
    // Out-of-migration guard (deletion-completeness design, T2): a migration
    // that adds a new agent-bound table (agent_id/agent_name column) without
    // updating the rename/delete constants in `agents::crud` would otherwise
    // leak orphan rows silently on agent rename/delete. Mirrors
    // `crud::tests::test_every_agent_binding_is_classified`, but surfaced
    // operationally against the live schema instead of only at PR time.
    let atc_db = infra.db.clone();
    let agent_table_classification_check = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        async move {
            let start = std::time::Instant::now();
            let rows: Result<Vec<(String,)>, sqlx::Error> = sqlx::query_as(
                "SELECT DISTINCT table_name FROM information_schema.columns \
                 WHERE table_schema='public' AND column_name IN ('agent_id','agent_name')",
            )
            .fetch_all(&atc_db)
            .await;
            let ms = start.elapsed().as_millis() as u64;
            match rows {
                Ok(rows) => {
                    let table_names: Vec<String> = rows.into_iter().map(|(t,)| t).collect();
                    let unclassified =
                        crate::gateway::handlers::agents::unclassified_agent_tables(&table_names);
                    if unclassified.is_empty() {
                        CheckResult::ok("all agent-bound tables classified", ms)
                    } else {
                        let mut cr = CheckResult::warn(
                            format!("{} unclassified agent-bound table(s)", unclassified.len()),
                            ms,
                            Some("classify new table(s) in agents/crud.rs (Ephemeral/History/DropRipe)".into()),
                        );
                        cr.details = Some(json!({"unclassified": unclassified}));
                        cr
                    }
                }
                Err(e) => CheckResult::error(
                    format!("failed to introspect agent-bound tables: {e}"),
                    ms,
                    Some("check database connectivity".into()),
                ),
            }
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("agent_table_classification"));

    // ── 15. Network discovery check ──────────────────────────────────────────
    let network_check = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            let start = std::time::Instant::now();
            let summary = super::super::network::fetch_network_summary(&status).await;
            let ms = start.elapsed().as_millis() as u64;
            let mut cr = CheckResult::ok("network discovery available", ms);
            cr.details = Some(summary);
            cr
        },
    )
    .await
    .unwrap_or_else(|_| CheckResult::timeout("network"));

    // ── Backup status check ─────────────────────────────────────────────────
    let backup_check = {
        let config = cfg_svc.shared_config.read().await;
        let backup_cfg = &config.backup;
        let enabled = backup_cfg.enabled;
        let cron = backup_cfg.cron.clone();
        let retention_days = backup_cfg.retention_days;
        drop(config);

        if enabled {
            // Find most recent backup file (current format: opex-YYYY-MM-DD.tar.gz).
            let mut latest: Option<(String, u64, chrono::DateTime<chrono::Utc>)> = None;
            if let Ok(mut dir) = tokio::fs::read_dir("backups").await {
                while let Ok(Some(entry)) = dir.next_entry().await {
                    let path = entry.path();
                    let is_backup = path.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".tar.gz"));
                    if is_backup
                        && let Ok(meta) = entry.metadata().await {
                            let modified = meta.modified().ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .and_then(|d| chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0));
                            if let Some(ts) = modified
                                && latest.as_ref().is_none_or(|(_, _, prev)| ts > *prev) {
                                    latest = Some((
                                        path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                                        meta.len(),
                                        ts,
                                    ));
                                }
                        }
                }
            }

            if let Some((filename, size_bytes, created_at)) = latest {
                let age_hours = (chrono::Utc::now() - created_at).num_hours();
                let status = if age_hours > 48 {
                    CheckStatus::Warn
                } else {
                    CheckStatus::Ok
                };
                let message = format!("last backup: {} ({} ago)", filename,
                    if age_hours < 1 { "< 1h".to_string() }
                    else if age_hours < 24 { format!("{age_hours}h") }
                    else { format!("{}d", age_hours / 24) }
                );
                let mut cr = CheckResult {
                    status,
                    message,
                    latency_ms: None,
                    fix_hint: if age_hours > 48 { Some("backup is stale — check cron schedule or run POST /api/backup".into()) } else { None },
                    details: None,
                };
                cr.details = Some(json!({
                    "enabled": true,
                    "cron": cron,
                    "retention_days": retention_days,
                    "last_backup": filename,
                    "last_backup_at": created_at,
                    "size_bytes": size_bytes,
                }));
                cr
            } else {
                let mut cr = CheckResult::warn(
                    "no backups found",
                    0,
                    Some("run POST /api/backup to create first backup".into()),
                );
                cr.details = Some(json!({
                    "enabled": true,
                    "cron": cron,
                    "retention_days": retention_days,
                }));
                cr
            }
        } else {
            let mut cr = CheckResult::warn(
                "automatic backups disabled",
                0,
                Some("enable in opex.toml: [backup] enabled = true".into()),
            );
            cr.details = Some(json!({
                "enabled": false,
                "cron": cron,
                "retention_days": retention_days,
            }));
            cr
        }
    };

    // ── Compute overall status ─────────────────────────────────────────────
    let all_checks = [
        &database_check,
        &toolgate_check,
        &migrations_check,
        &pgvector_check,
        &memory_worker_check,
        &disk_check,
        &browser_renderer_check,
        &secrets_check,
        &channels_check,
        &agents_check,
        &tool_health_check,
        &providers_check,
        &security_check,
        &network_check,
        &backup_check,
        &agent_table_classification_check,
    ];
    let all_ok = all_checks.iter().all(|c| !matches!(c.status, CheckStatus::Error));

    Json(json!({
        "ok": all_ok,
        "checks": {
            "database": database_check,
            "toolgate": toolgate_check,
            "migrations": migrations_check,
            "pgvector": pgvector_check,
            "memory_worker": memory_worker_check,
            "disk": disk_check,
            "browser_renderer": browser_renderer_check,
            "secrets": secrets_check,
            "channels": channels_check,
            "agents": agents_check,
            "tool_health": tool_health_check,
            "providers": providers_check,
            "security_audit": security_check,
            "network": network_check,
            "backup": backup_check,
            "agent_table_classification": agent_table_classification_check,
        }
    }))
}

#[cfg(test)]
mod tests {
    // Regression coverage for `check_security_audit`. The walker is heavy
    // (filesystem + tokio::spawn_blocking + tokio runtime), but the
    // detection logic is just regex matching. These tests pin the
    // patterns so accidental edits — e.g., dropping a length anchor —
    // surface immediately.

    fn cred_patterns() -> Vec<(regex::Regex, &'static str)> {
        // Mirrors `check_security_audit::patterns`. Keep in sync with that
        // table.
        let raw: &[(&str, &str)] = &[
            (r"sk-[a-zA-Z0-9]{40,}", "OpenAI key"),
            (r"ghp_[a-zA-Z0-9]{36}", "GitHub token"),
            (r"AIza[0-9A-Za-z\-_]{35}", "Google API key"),
            (r#"[Aa][Pp][Ii][_-]?[Kk][Ee][Yy]\s*[:=]\s*['"]?[a-zA-Z0-9]{20,}"#, "generic API key"),
        ];
        raw.iter()
            .map(|(p, n)| (regex::Regex::new(p).expect("regex compiles"), *n))
            .collect()
    }

    #[test]
    fn detects_openai_key() {
        let pats = cred_patterns();
        let s = "leak: sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ in code";
        assert!(pats[0].0.is_match(s));
    }

    #[test]
    fn detects_github_token() {
        let pats = cred_patterns();
        let s = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB and noise";
        assert!(pats[1].0.is_match(s));
    }

    #[test]
    fn detects_google_api_key() {
        let pats = cred_patterns();
        let s = "key=AIzaSyDdRhAB-abcdefghij0123456789klmno_q";
        assert!(pats[2].0.is_match(s));
    }

    #[test]
    fn detects_generic_api_key_assignment() {
        let pats = cred_patterns();
        let s = "API_KEY=abcdefghijklmnopqrstuvwxyz0123";
        assert!(pats[3].0.is_match(s));
    }

    #[test]
    fn does_not_match_short_secret_lookalikes() {
        let pats = cred_patterns();
        // OpenAI prefix but well below the 40-char minimum.
        assert!(!pats[0].0.is_match("sk-tooShort"));
        // GitHub prefix with a wrong-length suffix.
        assert!(!pats[1].0.is_match("ghp_tooShort"));
    }

    #[test]
    fn does_not_match_unrelated_text() {
        let pats = cred_patterns();
        let s = "this string has no credentials at all";
        for (re, _) in &pats {
            assert!(!re.is_match(s), "pattern false-positive: {}", re.as_str());
        }
    }

    #[test]
    fn probe_status_reachable_accepts_any_response_including_404() {
        use super::probe_status_reachable;
        // A response — any response — proves the endpoint is reachable. 404/405
        // are legitimate for non-OpenAI providers (e.g. a TTS) hit at /v1/models.
        for ok in [200u16, 204, 401, 403, 404, 405] {
            assert!(probe_status_reachable(ok), "status {ok} must count as reachable");
        }
        // Server up but unhealthy / wrong → not reachable-ok (caller warns).
        for bad in [400u16, 429, 500, 502, 503] {
            assert!(!probe_status_reachable(bad), "status {bad} must not count as reachable");
        }
    }

    #[test]
    fn probe_uses_direct_client_only_for_loopback() {
        use super::probe_uses_direct_client;
        assert!(probe_uses_direct_client("http://localhost:8088"), "localhost loopback → direct client");
        assert!(probe_uses_direct_client("http://127.0.0.1:8088"), "127.0.0.1 loopback → direct client");
        assert!(!probe_uses_direct_client("https://openrouter.ai/api/v1"), "external → SSRF client");
        assert!(!probe_uses_direct_client("http://10.10.1.42:8000"), "private IP literal → keep SSRF client");
    }
}
