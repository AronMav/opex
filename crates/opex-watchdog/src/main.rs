mod checker;
mod proc;
mod recovery;
mod resources;
mod status;
// alerter, config, and inactivity live in the lib crate (src/lib.rs);
// sharing them with the lib avoids compiling duplicate copies into the
// binary (and the dead_code warnings that go with it).

use opex_watchdog::{alerter, config, inactivity, infra_jobs};
use opex_watchdog::infra_watch::{classify, is_excluded, should_trigger, ContainerClass};

use std::collections::HashMap;

/// Consecutive-cycle grace period before a `Problem`-classified container
/// triggers a self-healing infra-event POST.
const INFRA_GRACE: u32 = 2;

/// Reads `OPEX_<suffix>`. Local copy — watchdog intentionally has no dep on
/// opex-gateway-util.
fn env_var(suffix: &str) -> Option<String> {
    std::env::var(format!("OPEX_{suffix}")).ok()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("opex_watchdog=info".parse()?),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/watchdog.toml".into());
    let mut cfg = config::load_config(&config_path)?;
    tracing::info!(
        checks = cfg.checks.len(),
        "watchdog loaded config from {}",
        config_path
    );

    if !cfg.watchdog.enabled {
        tracing::info!("watchdog disabled, exiting");
        return Ok(());
    }

    let auth_token = env_var("AUTH_TOKEN").unwrap_or_default();
    if auth_token.is_empty() {
        tracing::warn!("OPEX_AUTH_TOKEN not set — alerts will fail");
    }

    let core_url = env_var("CORE_URL").unwrap_or_else(|| "http://localhost:18789".into());

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let alerter = alerter::Alerter::new(&core_url, &auth_token);
    let mut alert_config = alerter.fetch_config().await.unwrap_or_default();
    tracing::info!(
        channels = alert_config.channel_ids.len(),
        events = ?alert_config.events,
        "alert config loaded from API"
    );
    let mut recovery_state = recovery::RecoveryState::new();
    let mut check_statuses: HashMap<String, status::ServiceStatus> = HashMap::new();
    let mut was_down: HashMap<String, bool> = HashMap::new();
    // F048: dedup the "flapping — restarts stopped" alert (mirrors was_down) so
    // it fires once on the transition into flapping, not every loop iteration.
    let mut was_flapping: HashMap<String, bool> = HashMap::new();
    let mut resource_status: Option<resources::ResourceStatus> = None;
    let mut was_resource_warning: HashMap<String, bool> = HashMap::new();
    let mut was_container_unhealthy: HashMap<String, bool> = HashMap::new();
    let mut unhealthy_streak: HashMap<String, u32> = HashMap::new();
    let mut inactivity_state: HashMap<inactivity::EpisodeKey, inactivity::AlertState> =
        HashMap::new();
    let start_time = std::time::Instant::now();
    // Initialize to force immediate check on first iteration
    let mut resource_timer = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(3600))
        .unwrap_or(start_time);
    // Infra-jobs backstop: daily timer (init in the past → immediate first check)
    // + transition flag so we alert once per disabled-state, not every cycle.
    let mut jobs_timer = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(86_400))
        .unwrap_or(start_time);
    let mut jobs_alerted = false;

    // Notify systemd we're ready
    #[cfg(target_os = "linux")]
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);

    // Grace period
    tracing::info!(secs = cfg.watchdog.grace_period_secs, "grace period");
    tokio::time::sleep(std::time::Duration::from_secs(
        cfg.watchdog.grace_period_secs,
    ))
    .await;
    tracing::info!("grace period ended, starting checks");

    loop {
        // Hot-reload config
        if let Ok(new_cfg) = config::load_config(&config_path) {
            cfg = new_cfg;
        }

        // Refresh alert config from API (fallback to cached on failure)
        if let Some(new_alert_cfg) = alerter.fetch_config().await {
            alert_config = new_alert_cfg;
        }

        if !cfg.watchdog.enabled {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }

        // Service health checks
        for check in &cfg.checks {
            if !check.enabled {
                continue;
            }

            let result = checker::run_check(check, &http).await;
            let previously_down = was_down.get(&check.name).copied().unwrap_or(false);

            if result.ok {
                if previously_down {
                    recovery_state.mark_recovered(&check.name);
                    was_down.insert(check.name.clone(), false);
                    was_flapping.insert(check.name.clone(), false); // F048: reset on recovery
                    alerter
                        .send(
                            &alert_config,
                            &format!("✅ {} recovered", check.name),
                            "recovery",
                        )
                        .await;
                }
                check_statuses.insert(
                    check.name.clone(),
                    status::ServiceStatus {
                        ok: true,
                        latency_ms: result.latency_ms,
                        last_restart: None,
                        error: None,
                        flapping: false,
                        can_restart: check.restart_cmd.is_some(),
                    },
                );
            } else {
                tracing::warn!(service = %check.name, error = ?result.error, "check failed");
                was_down.insert(check.name.clone(), true);

                let mut last_restart = None;
                let mut flapping = false;

                if let Some(ref restart_cmd) = check.restart_cmd {
                    if recovery_state.can_restart(
                        &check.name,
                        cfg.watchdog.flap_window_secs,
                        cfg.watchdog.flap_threshold,
                    ) {
                        let ok = recovery::restart_service(restart_cmd).await;
                        last_restart = Some(chrono::Utc::now().to_rfc3339());
                        if ok {
                            alerter
                                .send(
                                    &alert_config,
                                    &format!("🔄 {} restarted", check.name),
                                    "restart",
                                )
                                .await;
                        }
                    } else if recovery_state.is_flapping(&check.name) {
                        flapping = true;
                        // F048: only alert on the transition INTO flapping, not
                        // every 60s cycle while it stays flapping.
                        if !was_flapping.get(&check.name).copied().unwrap_or(false) {
                            alerter
                                .send(
                                    &alert_config,
                                    &format!("🔥 {} flapping — restarts stopped", check.name),
                                    "down",
                                )
                                .await;
                            was_flapping.insert(check.name.clone(), true);
                        }
                        recovery_state.enter_cooldown(&check.name, cfg.watchdog.cooldown_secs);
                    }
                } else if !previously_down {
                    alerter
                        .send(
                            &alert_config,
                            &format!(
                                "⚠️ {} down: {}",
                                check.name,
                                result.error.as_deref().unwrap_or("?")
                            ),
                            "down",
                        )
                        .await;
                }

                // Always update status so the status file reflects current state
                check_statuses.insert(
                    check.name.clone(),
                    status::ServiceStatus {
                        ok: false,
                        latency_ms: result.latency_ms,
                        last_restart,
                        error: result.error,
                        flapping,
                        can_restart: check.restart_cmd.is_some(),
                    },
                );
            }
        }

        // Resource checks (less frequent)
        if resource_timer.elapsed().as_secs() >= cfg.resources.check_interval_secs {
            resource_timer = std::time::Instant::now();
            let res =
                resources::check_resources(&cfg.resources, &http, &core_url, &auth_token).await;

            // Alert only on state transitions (first occurrence or severity change)
            if res.disk_critical
                && !was_resource_warning
                    .get("disk_critical")
                    .copied()
                    .unwrap_or(false)
            {
                alerter
                    .send(
                        &alert_config,
                        &format!("🚨 Disk critical: {:.1}GB free", res.disk_free_gb),
                        "resource",
                    )
                    .await;
                was_resource_warning.insert("disk_critical".into(), true);
                was_resource_warning.remove("disk_warning");
            } else if res.disk_warning
                && !res.disk_critical
                && !was_resource_warning
                    .get("disk_warning")
                    .copied()
                    .unwrap_or(false)
            {
                alerter
                    .send(
                        &alert_config,
                        &format!("⚠️ Disk low: {:.1}GB free", res.disk_free_gb),
                        "resource",
                    )
                    .await;
                was_resource_warning.insert("disk_warning".into(), true);
                was_resource_warning.remove("disk_critical");
            } else if !res.disk_warning && !res.disk_critical {
                was_resource_warning.remove("disk_warning");
                was_resource_warning.remove("disk_critical");
            }

            // F078: mirror the disk path — alert on ram_warning too, not just
            // ram_critical. Previously ram_warning_percent silently did nothing
            // and the first RAM alert fired only at ram_critical (~95%), by
            // which point the host may already be OOM-killing.
            if res.ram_critical
                && !was_resource_warning
                    .get("ram_critical")
                    .copied()
                    .unwrap_or(false)
            {
                alerter
                    .send(
                        &alert_config,
                        &format!("🚨 RAM critical: {:.0}%", res.ram_used_percent),
                        "resource",
                    )
                    .await;
                was_resource_warning.insert("ram_critical".into(), true);
                was_resource_warning.remove("ram_warning");
            } else if res.ram_warning
                && !res.ram_critical
                && !was_resource_warning
                    .get("ram_warning")
                    .copied()
                    .unwrap_or(false)
            {
                alerter
                    .send(
                        &alert_config,
                        &format!("⚠️ RAM high: {:.0}%", res.ram_used_percent),
                        "resource",
                    )
                    .await;
                was_resource_warning.insert("ram_warning".into(), true);
                was_resource_warning.remove("ram_critical");
            } else if !res.ram_warning && !res.ram_critical {
                was_resource_warning.remove("ram_warning");
                was_resource_warning.remove("ram_critical");
            }

            resource_status = Some(res);
        }

        // ── Agent inactivity check ──────────────────────────────────────
        if let Err(e) = inactivity::tick(
            &http,
            &core_url,
            &auth_token,
            &cfg.watchdog,
            &mut inactivity_state,
            &alerter,
            &alert_config,
        )
        .await
        {
            tracing::warn!(error = %e, "inactivity tick failed");
        }

        // ── Infra-jobs backstop (backup + curator enabled?) — daily ─────
        if jobs_timer.elapsed().as_secs() >= 86_400 {
            jobs_timer = std::time::Instant::now();
            match infra_jobs::tick(
                &http,
                &core_url,
                &auth_token,
                &alerter,
                &alert_config,
                jobs_alerted,
            )
            .await
            {
                Ok(now) => jobs_alerted = now,
                Err(e) => tracing::warn!(error = %e, "infra-jobs check failed"),
            }
        }

        // ── Session auto-retry ──────────────────────────────────────────
        if cfg.watchdog.session_retry_enabled {
            match http
                .get(format!(
                    "{}/api/sessions/stuck?stale_secs={}&max_retries={}",
                    core_url,
                    cfg.watchdog.session_retry_stale_secs,
                    cfg.watchdog.session_retry_max_attempts
                ))
                .header("Authorization", format!("Bearer {}", auth_token))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<serde_json::Value>().await
                        && let Some(sessions) = body.get("sessions").and_then(|s| s.as_array())
                    {
                        for s in sessions {
                            let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let agent = s.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                            tracing::warn!(session_id = sid, agent, "retrying stuck session");
                            // R-RETRY fix: the retry endpoint requires `?agent=`
                            // (IDOR hardening, commit 1b716207). Without it every
                            // POST returned 400 and the auto-retry feature was
                            // silently dead. `agent` comes from the stuck-session
                            // row's agent_id, so verify_session_agent will match.
                            // `.query()` URL-encodes the value correctly.
                            match http
                                .post(format!("{}/api/sessions/{}/retry", core_url, sid))
                                .query(&[("agent", agent)])
                                .header("Authorization", format!("Bearer {}", auth_token))
                                .send()
                                .await
                            {
                                Ok(r) if r.status().is_success() => {
                                    tracing::info!(session_id = sid, "retry request accepted");
                                    alerter
                                        .send(
                                            &alert_config,
                                            &format!(
                                                "Auto-retrying stuck session {} (agent: {})",
                                                sid, agent
                                            ),
                                            "session_retry",
                                        )
                                        .await;
                                }
                                Ok(r) => {
                                    let status = r.status();
                                    let body = r.text().await.unwrap_or_default();
                                    tracing::error!(session_id = sid, %status, body, "retry request failed");
                                }
                                Err(e) => {
                                    tracing::error!(session_id = sid, error = %e, "retry request error")
                                }
                            }
                        }
                    }
                }
                // F116: a non-2xx (endpoint renamed/removed, 500 during core
                // restart, auth drift) used to be swallowed silently and the
                // transport error logged only at debug — the auto-retry feature
                // could die invisibly. Surface both at warn.
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(%status, body, "stuck-sessions check returned non-success");
                }
                Err(e) => tracing::warn!(error = %e, "stuck sessions check failed"),
            }
        }

        // Docker container check
        let all_containers = checker::check_docker_containers().await;
        // Alert only for newly unhealthy containers (not already alerted)
        let mut current_unhealthy: HashMap<String, bool> = HashMap::new();
        let mut newly_unhealthy: Vec<String> = Vec::new();
        for c in &all_containers {
            if !c.healthy {
                current_unhealthy.insert(c.docker_name.clone(), true);
                if !was_container_unhealthy
                    .get(&c.docker_name)
                    .copied()
                    .unwrap_or(false)
                {
                    newly_unhealthy.push(c.name.clone());
                }
            }
        }
        if !newly_unhealthy.is_empty() {
            let msg = format!("🐳 Unhealthy containers: {}", newly_unhealthy.join(", "));
            alerter.send(&alert_config, &msg, "down").await;
        }
        was_container_unhealthy = current_unhealthy;

        // ── Self-healing: устойчиво-проблемные контейнеры → триггер Opex ──
        let mut next_streak: HashMap<String, u32> = HashMap::new();
        for c in &all_containers {
            if is_excluded(&c.docker_name) {
                continue;
            }
            let class = classify(&c.status);
            if class == ContainerClass::Problem {
                let streak = unhealthy_streak.get(&c.docker_name).copied().unwrap_or(0) + 1;
                next_streak.insert(c.docker_name.clone(), streak);
                if should_trigger(class, streak, INFRA_GRACE) {
                    alerter.post_infra_event(&c.docker_name, &c.status).await;
                }
            }
            // Healthy/Transient → streak сбрасывается (не переносим в next_streak).
        }
        unhealthy_streak = next_streak;

        // Write status file
        status::write_status(&status::WatchdogStatus {
            last_check: chrono::Utc::now().to_rfc3339(),
            uptime_secs: start_time.elapsed().as_secs(),
            checks: check_statuses.clone(),
            resources: resource_status.clone(),
            containers: all_containers,
        });

        // Notify systemd watchdog (keeps WatchdogSec alive)
        #[cfg(target_os = "linux")]
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);

        tokio::time::sleep(std::time::Duration::from_secs(cfg.watchdog.interval_secs)).await;
    }
}
