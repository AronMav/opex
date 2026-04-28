mod alerter;
mod checker;
mod config;
mod recovery;
mod resources;
mod status;

use std::collections::HashMap;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("hydeclaw_watchdog=info".parse()?),
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

    let auth_token = std::env::var("HYDECLAW_AUTH_TOKEN").unwrap_or_default();
    if auth_token.is_empty() {
        tracing::warn!("HYDECLAW_AUTH_TOKEN not set — alerts will fail");
    }

    let core_url = std::env::var("HYDECLAW_CORE_URL")
        .unwrap_or_else(|_| "http://localhost:18789".into());

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
    let mut resource_status: Option<resources::ResourceStatus> = None;
    let mut was_resource_warning: HashMap<String, bool> = HashMap::new();
    let mut was_container_unhealthy: HashMap<String, bool> = HashMap::new();
    let start_time = std::time::Instant::now();
    // Initialize to force immediate check on first iteration
    let mut resource_timer = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(3600))
        .unwrap_or(start_time);

    // Notify systemd we're ready
    #[cfg(target_os = "linux")]
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);

    // Grace period
    tracing::info!(secs = cfg.watchdog.grace_period_secs, "grace period");
    tokio::time::sleep(std::time::Duration::from_secs(cfg.watchdog.grace_period_secs)).await;
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
                        alerter
                            .send(
                                &alert_config,
                                &format!("🔥 {} flapping — restarts stopped", check.name),
                                "down",
                            )
                            .await;
                        recovery_state
                            .enter_cooldown(&check.name, cfg.watchdog.cooldown_secs);
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
            if res.disk_critical && !was_resource_warning.get("disk_critical").copied().unwrap_or(false) {
                alerter.send(&alert_config, &format!("🚨 Disk critical: {:.1}GB free", res.disk_free_gb), "resource").await;
                was_resource_warning.insert("disk_critical".into(), true);
                was_resource_warning.remove("disk_warning");
            } else if res.disk_warning && !res.disk_critical && !was_resource_warning.get("disk_warning").copied().unwrap_or(false) {
                alerter.send(&alert_config, &format!("⚠️ Disk low: {:.1}GB free", res.disk_free_gb), "resource").await;
                was_resource_warning.insert("disk_warning".into(), true);
                was_resource_warning.remove("disk_critical");
            } else if !res.disk_warning && !res.disk_critical {
                was_resource_warning.remove("disk_warning");
                was_resource_warning.remove("disk_critical");
            }

            if res.ram_critical && !was_resource_warning.get("ram_critical").copied().unwrap_or(false) {
                alerter.send(&alert_config, &format!("🚨 RAM critical: {:.0}%", res.ram_used_percent), "resource").await;
                was_resource_warning.insert("ram_critical".into(), true);
            } else if !res.ram_critical {
                was_resource_warning.remove("ram_critical");
            }


            resource_status = Some(res);
        }

        // ── Session auto-retry ──────────────────────────────────────────
        if cfg.watchdog.session_retry_enabled {
            match http
                .get(format!("{}/api/sessions/stuck?stale_secs={}&max_retries={}",
                    core_url, cfg.watchdog.session_retry_stale_secs, cfg.watchdog.session_retry_max_attempts))
                .header("Authorization", format!("Bearer {}", auth_token))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<serde_json::Value>().await
                        && let Some(sessions) = body.get("sessions").and_then(|s| s.as_array()) {
                        for s in sessions {
                            let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let agent = s.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                            tracing::warn!(session_id = sid, agent, "retrying stuck session");
                            match http
                                .post(format!("{}/api/sessions/{}/retry", core_url, sid))
                                .header("Authorization", format!("Bearer {}", auth_token))
                                .send()
                                .await
                            {
                                Ok(r) if r.status().is_success() => {
                                    tracing::info!(session_id = sid, "retry request accepted");
                                    alerter.send(&alert_config,
                                        &format!("Auto-retrying stuck session {} (agent: {})", sid, agent),
                                        "session_retry",
                                    ).await;
                                }
                                Ok(r) => {
                                    let status = r.status();
                                    let body = r.text().await.unwrap_or_default();
                                    tracing::error!(session_id = sid, %status, body, "retry request failed");
                                }
                                Err(e) => tracing::error!(session_id = sid, error = %e, "retry request error"),
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "stuck sessions check failed"),
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
                if !was_container_unhealthy.get(&c.docker_name).copied().unwrap_or(false) {
                    newly_unhealthy.push(c.name.clone());
                }
            }
        }
        if !newly_unhealthy.is_empty() {
            let msg = format!("🐳 Unhealthy containers: {}", newly_unhealthy.join(", "));
            alerter.send(&alert_config, &msg, "down").await;
        }
        was_container_unhealthy = current_unhealthy;

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
