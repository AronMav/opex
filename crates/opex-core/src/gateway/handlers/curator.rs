use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use serde::Deserialize;

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, ConfigServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/curator/status", get(api_curator_status))
        .route("/api/curator/config", get(api_curator_config_get).put(api_curator_config_put))
        .route("/api/curator/run", axum::routing::post(api_curator_run))
        .route("/api/curator/runs", get(api_curator_runs))
}

// ── GET /api/curator/status ───────────────────────────────────────────────────

pub(crate) async fn api_curator_status(
    State(infra): State<InfraServices>,
    State(cfg_svc): State<ConfigServices>,
) -> impl IntoResponse {
    let last = match crate::db::curator_runs::last_run(&infra.db).await {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(error = %e, "curator status DB query failed");
            None
        }
    };
    let (enabled, cron) = {
        let s = cfg_svc.shared_config.read().await;
        (s.curator.enabled, s.curator.cron.clone())
    };

    Json(serde_json::json!({
        "enabled": enabled,
        "cron": cron,
        "last_run_at": last.as_ref().map(|r| r.started_at),
        "last_run_id": last.as_ref().map(|r| r.id),
        "last_phase1": last.as_ref().and_then(|r| r.phase1).unwrap_or(0),
        "last_phase2": last.as_ref().and_then(|r| r.phase2).unwrap_or(0),
        "last_phase3": last.as_ref().and_then(|r| r.phase3).unwrap_or(0),
    }))
}

// ── GET /api/curator/config ───────────────────────────────────────────────────

pub(crate) async fn api_curator_config_get(
    State(cfg_svc): State<ConfigServices>,
) -> impl IntoResponse {
    let shared = cfg_svc.shared_config.read().await;
    let cfg = &shared.curator;
    Json(serde_json::json!({
        "enabled":             cfg.enabled,
        "cron":                cfg.cron,
        "min_idle_minutes":    cfg.min_idle_minutes,
        "stale_after_days":    cfg.stale_after_days,
        "archive_after_days":  cfg.archive_after_days,
        "max_repairs_per_run": cfg.max_repairs_per_run,
        "agent_name":          cfg.agent_name,
    }))
}

// ── PUT /api/curator/config ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct CuratorConfigUpdate {
    enabled:             Option<bool>,
    cron:                Option<String>,
    min_idle_minutes:    Option<u32>,
    stale_after_days:    Option<u32>,
    archive_after_days:  Option<u32>,
    max_repairs_per_run: Option<u32>,
    agent_name:          Option<String>,
}

pub(crate) async fn api_curator_config_put(
    State(cfg_svc): State<ConfigServices>,
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    Json(body): Json<CuratorConfigUpdate>,
) -> impl IntoResponse {
    // Suppress the file-watcher reload cycle triggered by the write below,
    // since we hot-reload shared_config ourselves immediately after.
    cfg_svc.config_api_write_flag.store(true, std::sync::atomic::Ordering::Release);
    let config_path = opex_gateway_util::config_path::resolve_config_path();
    if let Err(e) = crate::config::update_curator_config(
        &config_path,
        body.enabled,
        body.cron.as_deref(),
        body.min_idle_minutes,
        body.stale_after_days,
        body.archive_after_days,
        body.max_repairs_per_run,
        body.agent_name.as_deref(),
    ) {
        // F113: the write we set the flag to suppress never happened. Reset it so
        // the flag isn't left armed — otherwise it consumes (and silently skips)
        // the operator's NEXT manual opex.toml edit.
        cfg_svc.config_api_write_flag.store(false, std::sync::atomic::Ordering::Release);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ).into_response();
    }
    // Hot-reload shared config so GET immediately reflects the new values
    let new_curator_cfg = match crate::config::AppConfig::load(&config_path) {
        Ok(new_config) => {
            let curator_cfg = new_config.curator.clone();
            let mut config = cfg_svc.shared_config.write().await;
            *config = new_config;
            Some(curator_cfg)
        }
        Err(e) => {
            tracing::warn!(error = %e, "curator config updated on disk but failed to reload into memory");
            None
        }
    };
    // Reschedule curator so new cron/enabled takes effect immediately
    if let Some(curator_cfg) = new_curator_cfg {
        if let Err(e) = agents.scheduler.reschedule_curator(
            curator_cfg.clone(),
            infra.db.clone(),
            agents.clone(),
        ).await {
            tracing::warn!(error = %e, "curator reschedule failed");
        } else {
            tracing::info!(cron = %curator_cfg.cron, enabled = curator_cfg.enabled, "curator rescheduled via API");
        }
    }
    Json(serde_json::json!({"ok": true})).into_response()
}

// ── POST /api/curator/run ─────────────────────────────────────────────────────

pub(crate) async fn api_curator_run(
    State(infra): State<InfraServices>,
    State(cfg_svc): State<ConfigServices>,
    State(agents): State<crate::gateway::clusters::AgentCore>,
) -> impl IntoResponse {
    let db = infra.db.clone();
    let cfg = cfg_svc.shared_config.read().await.curator.clone();

    if let Ok(Some(last)) = crate::db::curator_runs::last_run(&db).await
        && last.finished_at.is_none()
        // F074: only block on a RECENT run. A crash/restart mid-run (every
        // deploy restarts core) leaves finished_at NULL forever, and there is
        // no startup sweep — without this staleness bound the manual curator is
        // permanently bricked with a 409.
        && (chrono::Utc::now() - last.started_at) < chrono::Duration::minutes(30)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "curator already running", "run_id": last.id})),
        ).into_response();
    }

    let run_id = match crate::db::curator_runs::insert_run(&db, "manual", false).await {
        Ok(id) => id,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ).into_response(),
    };

    tokio::spawn(async move {
        match crate::curator::run_curator(&db, &cfg, std::sync::Arc::new(agents), crate::config::WORKSPACE_DIR, false).await {
            Ok(s) => {
                crate::db::curator_runs::finish_run(&db, run_id, s.phase1, s.phase2, s.phase3, Some(&s.report_md), None, false).await.ok();
            }
            Err(e) => {
                crate::db::curator_runs::finish_run(&db, run_id, 0, 0, 0, None, Some(&e.to_string()), false).await.ok();
            }
        }
    });

    Json(serde_json::json!({"run_id": run_id})).into_response()
}

// ── POST /api/curator/preview ────────────────────────────────────────────────

// ── GET /api/curator/runs ─────────────────────────────────────────────────────

use serde::Serialize;

#[derive(Serialize)]
struct CuratorRunResponse {
    id: uuid::Uuid,
    started_at: chrono::DateTime<chrono::Utc>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
    triggered_by: String,
    phase1_transitions: Option<i32>,
    phase2_repairs: Option<i32>,
    phase3_commands: Option<i32>,
    skipped_reason: Option<String>,
    report_md: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
}

pub(crate) async fn api_curator_runs(State(infra): State<InfraServices>) -> impl IntoResponse {
    match crate::db::curator_runs::list_runs(&infra.db, 50).await {
        Ok(runs) => {
            let response: Vec<CuratorRunResponse> = runs
                .into_iter()
                .map(|r| {
                    let duration_ms = r.finished_at.map(|finished| {
                        (finished - r.started_at).num_milliseconds()
                    });
                    CuratorRunResponse {
                        id: r.id,
                        started_at: r.started_at,
                        finished_at: r.finished_at,
                        triggered_by: r.trigger,
                        phase1_transitions: r.phase1,
                        phase2_repairs: r.phase2,
                        phase3_commands: r.phase3,
                        skipped_reason: r.skip_reason,
                        report_md: r.report_md,
                        error: r.error,
                        duration_ms,
                    }
                })
                .collect();
            Json(serde_json::json!({"runs": response})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

