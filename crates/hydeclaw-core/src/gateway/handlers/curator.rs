use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use uuid::Uuid;

use super::super::AppState;
use crate::gateway::clusters::{ConfigServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/curator/status", get(api_curator_status))
        .route("/api/curator/run", axum::routing::post(api_curator_run))
        .route("/api/curator/runs", get(api_curator_runs))
        .route("/api/curator/runs/{id}", get(api_curator_run_get))
}

// ── GET /api/curator/status ───────────────────────────────────────────────────

pub(crate) async fn api_curator_status(
    State(infra): State<InfraServices>,
    State(cfg_svc): State<ConfigServices>,
) -> impl IntoResponse {
    let last = crate::db::curator_runs::last_run(&infra.db).await.ok().flatten();
    let cfg = &cfg_svc.config.curator;

    Json(serde_json::json!({
        "enabled": cfg.enabled,
        "cron": cfg.cron,
        "last_run_at": last.as_ref().map(|r| r.started_at),
        "last_run_id": last.as_ref().map(|r| r.id),
        "last_phase1": last.as_ref().and_then(|r| r.phase1).unwrap_or(0),
        "last_phase2": last.as_ref().and_then(|r| r.phase2).unwrap_or(0),
        "last_phase3": last.as_ref().and_then(|r| r.phase3).unwrap_or(0),
    }))
}

// ── POST /api/curator/run ─────────────────────────────────────────────────────

pub(crate) async fn api_curator_run(
    State(infra): State<InfraServices>,
    State(cfg_svc): State<ConfigServices>,
) -> impl IntoResponse {
    let db = infra.db.clone();
    let cfg = cfg_svc.config.curator.clone();
    let secrets = infra.secrets.clone();

    // Reject if a run is already in progress (no finished_at means still running).
    if let Ok(Some(last)) = crate::db::curator_runs::last_run(&db).await {
        if last.finished_at.is_none() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "curator already running", "run_id": last.id})),
            )
                .into_response();
        }
    }

    let run_id = match crate::db::curator_runs::insert_run(&db, "manual").await {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    tokio::spawn(async move {
        match crate::curator::run_curator(&db, &cfg, secrets, crate::config::WORKSPACE_DIR).await {
            Ok(s) => {
                crate::db::curator_runs::finish_run(
                    &db,
                    run_id,
                    s.phase1,
                    s.phase2,
                    s.phase3,
                    Some(&s.report_md),
                    None,
                )
                .await
                .ok();
            }
            Err(e) => {
                crate::db::curator_runs::finish_run(
                    &db,
                    run_id,
                    0,
                    0,
                    0,
                    None,
                    Some(&e.to_string()),
                )
                .await
                .ok();
            }
        }
    });

    Json(serde_json::json!({"run_id": run_id})).into_response()
}

// ── GET /api/curator/runs ─────────────────────────────────────────────────────

pub(crate) async fn api_curator_runs(State(infra): State<InfraServices>) -> impl IntoResponse {
    match crate::db::curator_runs::list_runs(&infra.db, 50).await {
        Ok(runs) => Json(serde_json::json!({"runs": runs})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/curator/runs/{id} ────────────────────────────────────────────────

pub(crate) async fn api_curator_run_get(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match crate::db::curator_runs::get_run(&infra.db, id).await {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
