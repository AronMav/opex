mod config;
mod handlers;
#[cfg(feature = "otel")]
mod otel_init;
mod tasks;

use sqlx::postgres::{PgListener, PgPoolOptions};

/// Wake source for the hybrid LISTEN/poll loop.
///
/// REF-04: LISTEN is primary; poll is the 60-second safety net that reclaims
/// anything the listener missed (e.g. dropped socket during a NOTIFY burst).
/// `ListenerDied` signals that the listener connection errored and must be
/// rebuilt on the next iteration.
enum Wake {
    Notify,
    Poll,
    ListenerDied,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // Load .env from binary dir (memory-worker runs as separate binary)
    dotenv::dotenv().ok();

    // Tracing subscriber init — when the `otel` feature is built and
    // `OTEL_EXPORTER_OTLP_ENDPOINT` is set, spans flow to the same Jaeger
    // collector as opex-core (separate `service.name`). Otherwise
    // standard fmt-only logging.
    #[cfg(feature = "otel")]
    otel_init::init();
    #[cfg(not(feature = "otel"))]
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("opex_memory_worker=info".parse()?),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/opex.toml".into());
    let cfg = config::load_config(&config_path)?;

    if !cfg.worker.enabled {
        tracing::info!("memory worker disabled");
        return Ok(());
    }

    tracing::info!(
        toolgate_url = %cfg.toolgate_url,
        workspace_dir = %cfg.workspace_dir,
        fts_language = %cfg.fts_language,
        poll = cfg.worker.poll_interval_secs,
        notify_mode = ?cfg.worker.notify_mode,
        "memory worker starting"
    );

    let db = PgPoolOptions::new()
        .max_connections(3)
        .connect(&cfg.database_url)
        .await?;
    tracing::info!("database connected");

    // Recover stuck 'processing' tasks from previous crash
    let recovered = tasks::recover_stuck(&db).await?;
    if recovered > 0 {
        tracing::info!(recovered, "recovered stuck tasks from previous crash");
    }

    #[cfg(target_os = "linux")]
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);

    let poll = std::time::Duration::from_secs(cfg.worker.poll_interval_secs);

    // Shared embedding client — retry policy + W3C traceparent injection live
    // here; worker no longer manages its own reqwest::Client for embeddings.
    // `requested_dimensions = 0` means "let Toolgate resolve the active
    // embedding model's native dimension" (worker doesn't override dims).
    let toolgate_client = opex_embedding::ToolgateClient::new(cfg.toolgate_url.clone(), 0);

    let ctx = handlers::DispatchCtx {
        toolgate_client: &toolgate_client,
        workspace_dir: &cfg.workspace_dir,
        fts_language: &cfg.fts_language,
    };

    // ── REF-04: LISTEN/NOTIFY primary + poll safety net ─────────────────────
    //
    // Primary wake: PgListener on `memory_tasks_new`. Sub-100ms steady-state
    // pickup under normal operation (migration 023 trigger pg_notify's on every
    // INSERT commit).
    //
    // Safety net: poll every `poll_interval_secs` (HCS-4 preserved). Reclaims
    // anything that slipped through while LISTEN was dead (socket drop, burst
    // coalescing at the PG layer, etc.).

    let mut poll_tick = tokio::time::interval(poll);
    poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately — skip it so the initial wake is a real poll
    // after `poll_interval_secs`, not a tight loop at startup.
    poll_tick.tick().await;

    let mut listener: Option<PgListener> = if cfg.worker.notify_mode == config::NotifyMode::Listen {
        connect_listener(&cfg.database_url).await
    } else {
        tracing::info!("notify_mode = poll — skipping LISTEN, polling only");
        None
    };

    loop {
        // Readiness probe: don't dequeue tasks until Toolgate is up AND an
        // active embedding provider is configured. Avoids connection-error
        // spam at cold-start (worker would otherwise pull a task, call
        // embeddings, fail, retry — flooding logs). `fetch_health()` does not
        // retry by design (Task 6) — we pace ourselves via `poll` here.
        match toolgate_client.fetch_health().await {
            Ok(h) if h.active_embedding_provider.is_some() => {
                // OK — продолжаем к dequeue
            }
            Ok(_) => {
                tracing::debug!("toolgate up but no active embedding provider, waiting");
                tokio::time::sleep(poll).await;
                continue;
            }
            Err(e) => {
                tracing::debug!(error = %e, "toolgate health check failed, waiting");
                tokio::time::sleep(poll).await;
                continue;
            }
        }

        // Wait for EITHER a NOTIFY or the poll tick (catch-up safety net).
        let wake = match &mut listener {
            Some(l) => {
                tokio::select! {
                    notif = l.recv() => match notif {
                        Ok(_n) => Wake::Notify,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "PgListener recv failed; will reconnect on next iteration"
                            );
                            Wake::ListenerDied
                        }
                    },
                    _ = poll_tick.tick() => Wake::Poll,
                }
            }
            None => {
                poll_tick.tick().await;
                Wake::Poll
            }
        };

        // Reclaim a listener if it died and operators want LISTEN mode.
        // Drop the broken one first, then attempt a fresh connect. If the
        // reconnect fails, `listener` becomes `None` and the next iteration
        // relies on the poll tick until the next attempt.
        //
        // ALSO retry when `listener` is `None` and the wake is `Wake::Poll`
        // — covers the case where the INITIAL `connect_listener` at startup
        // returned `None` (e.g., transient DB unavailability). Without this,
        // the worker would silently stay poll-only for its lifetime, defeating
        // REF-04 sub-100ms pickup. See code review 2026-04-17.
        let should_retry_listen = cfg.worker.notify_mode == config::NotifyMode::Listen
            && (matches!(wake, Wake::ListenerDied)
                || (matches!(wake, Wake::Poll) && listener.is_none()));
        if should_retry_listen {
            drop(listener.take());
            listener = connect_listener(&cfg.database_url).await;
            if listener.is_some() {
                tracing::info!("PgListener reconnected");
            }
            // Fall through and drain pending tasks unconditionally — the poll
            // path is still responsible for catch-up.
        }

        // Drain pending work: NOTIFY may coalesce bursts at the PG layer, so
        // one recv() can correspond to N new tasks. Poll ticks use the same
        // drain to catch up anything that slipped through.
        loop {
            match tasks::claim_next(&db).await {
                Ok(Some(task)) => {
                    tracing::info!(id = %task.id, task_type = %task.task_type, "processing task");
                    match handlers::dispatch(&task, &db, &ctx).await {
                        Ok(result) => {
                            // F049: do NOT `?` here. A transient Postgres blip
                            // (prod PG restarts under deploy RAM pressure) on
                            // this UPDATE would otherwise propagate out of main()
                            // and KILL the worker, leaving the row 'processing'
                            // so recover_stuck redoes the whole (expensive)
                            // reindex after restart. Log + continue; the row
                            // stays 'processing' and the next poll / recover_stuck
                            // reclaims it — matching how claim/dispatch errors are
                            // already handled.
                            if let Err(e) = tasks::complete(&db, task.id, result).await {
                                tracing::error!(id = %task.id, error = %e, "failed to mark task complete (will be reclaimed)");
                            } else {
                                tracing::info!(id = %task.id, "task completed");
                            }
                        }
                        Err(e) => {
                            if let Err(fe) = tasks::fail(&db, task.id, &e.to_string()).await {
                                tracing::error!(id = %task.id, error = %fe, "failed to mark task failed (will be reclaimed)");
                            }
                            tracing::error!(id = %task.id, error = %e, "task failed");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %e, "failed to claim task");
                    break;
                }
            }
        }

        #[cfg(target_os = "linux")]
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
    }
}

/// Build a `PgListener` subscribed to `memory_tasks_new`.
///
/// Returns `None` on any failure (connect, subscribe) so the caller falls back
/// to pure-polling mode for this iteration and retries on the next poll tick.
/// Failures are logged at WARN so operators can spot persistent LISTEN issues.
async fn connect_listener(database_url: &str) -> Option<PgListener> {
    match PgListener::connect(database_url).await {
        Ok(mut l) => match l.listen("memory_tasks_new").await {
            Ok(()) => {
                tracing::info!("LISTEN memory_tasks_new active");
                Some(l)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "listener.listen(memory_tasks_new) failed; falling back to poll-only this cycle"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "PgListener::connect failed; falling back to poll-only this cycle"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit-level tests for the LISTEN/NOTIFY reconnect logic.
    //!
    //! We re-implement the should_retry_listen predicate exactly — tests
    //! protect the decision semantics from regression even without a live
    //! PostgreSQL. The integration test `integration_memory_worker_notify`
    //! covers the end-to-end wake-up path.

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Wake {
        Notify,
        Poll,
        ListenerDied,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum NotifyMode {
        Listen,
        Poll,
    }

    fn should_retry_listen(notify_mode: NotifyMode, wake: Wake, listener_is_some: bool) -> bool {
        notify_mode == NotifyMode::Listen
            && (matches!(wake, Wake::ListenerDied)
                || (matches!(wake, Wake::Poll) && !listener_is_some))
    }

    /// Regression for code review 2026-04-17: if the INITIAL `connect_listener`
    /// returns `None`, the worker must retry on the next `Wake::Poll` tick.
    /// Prior bug: reconnect was gated only on `Wake::ListenerDied`, so a
    /// failed-at-startup worker silently stayed poll-only for its lifetime.
    #[test]
    fn retry_when_listener_is_none_and_poll_wake() {
        assert!(
            should_retry_listen(NotifyMode::Listen, Wake::Poll, false),
            "poll tick with no listener must trigger reconnect in Listen mode"
        );
    }

    #[test]
    fn retry_on_listener_died() {
        assert!(
            should_retry_listen(NotifyMode::Listen, Wake::ListenerDied, true),
            "listener death must always trigger reconnect in Listen mode"
        );
    }

    #[test]
    fn no_retry_in_poll_mode() {
        assert!(
            !should_retry_listen(NotifyMode::Poll, Wake::Poll, false),
            "Poll mode must never attempt LISTEN reconnect"
        );
        assert!(
            !should_retry_listen(NotifyMode::Poll, Wake::ListenerDied, false),
            "Poll mode must ignore ListenerDied (cannot happen anyway)"
        );
    }

    #[test]
    fn no_retry_on_healthy_poll_tick() {
        // listener_is_some + Wake::Poll means the listener is working fine and
        // the poll tick is acting as the catch-up safety net — NO reconnect.
        assert!(
            !should_retry_listen(NotifyMode::Listen, Wake::Poll, true),
            "healthy poll tick with active listener must not re-connect"
        );
    }

    #[test]
    fn no_retry_on_successful_notify() {
        assert!(
            !should_retry_listen(NotifyMode::Listen, Wake::Notify, true),
            "Wake::Notify means the listener delivered; no reconnect needed"
        );
    }
}
