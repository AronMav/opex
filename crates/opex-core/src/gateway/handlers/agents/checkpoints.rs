use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;

use crate::gateway::clusters::AgentCore;
use crate::gateway::state::AppState;

// ── DTOs (defined in leaf file, imported here) ────────────────────────────────

#[path = "checkpoints_dto_structs.rs"]
mod checkpoints_dto_structs;
pub use checkpoints_dto_structs::{CheckpointListDto, CheckpointMetaDto, RestoreReportDto};

// ── Additional request types ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RestoreBody {
    #[serde(default)]
    pub file: Option<String>,
}

// ── Resolve helper ────────────────────────────────────────────────────────────

/// Extract (checkpoint_mgr, workspace_dir) from AppState via AgentCore.deps.
async fn resolve_mgr(
    core: &AgentCore,
) -> (
    std::sync::Arc<crate::agent::checkpoint_manager::CheckpointManager>,
    String,
) {
    let d = core.deps.read().await;
    (d.checkpoint_mgr.clone(), d.workspace_dir.clone())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn api_list_checkpoints(
    State(core): State<AgentCore>,
    Path(name): Path<String>,
) -> Result<Json<CheckpointListDto>, StatusCode> {
    let (mgr, _ws) = resolve_mgr(&core).await;
    let enabled = mgr.enabled();
    let items = mgr
        .list_checkpoints(&name)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|m| CheckpointMetaDto {
            n: m.n,
            commit: m.commit,
            created: m.created,
            summary: m.summary,
        })
        .collect();
    Ok(Json(CheckpointListDto { enabled, items }))
}

async fn api_diff_checkpoint(
    State(core): State<AgentCore>,
    Path((name, n)): Path<(String, usize)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let (mgr, ws) = resolve_mgr(&core).await;
    let diff = mgr
        .diff(&name, &ws, n)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::json!({ "diff": diff })))
}

async fn api_restore_checkpoint(
    State(core): State<AgentCore>,
    Path((name, n)): Path<(String, usize)>,
    Json(body): Json<RestoreBody>,
) -> Result<Json<RestoreReportDto>, StatusCode> {
    let (mgr, ws) = resolve_mgr(&core).await;
    let rep = mgr
        .restore(&name, &ws, n, body.file.as_deref())
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(RestoreReportDto {
        n: rep.n,
        files: rep.files,
        new_checkpoint: rep.new_checkpoint,
    }))
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{name}/checkpoints", get(api_list_checkpoints))
        .route(
            "/api/agents/{name}/checkpoints/{n}/diff",
            get(api_diff_checkpoint),
        )
        .route(
            "/api/agents/{name}/checkpoints/{n}/restore",
            post(api_restore_checkpoint),
        )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_list_dto_serializes() {
        let dto = CheckpointListDto {
            enabled: true,
            items: vec![CheckpointMetaDto {
                n: 2,
                commit: "abc".into(),
                created: "2026-06-25T10:00:00+00:00".into(),
                summary: "1 file".into(),
            }],
        };
        let j = serde_json::to_value(&dto).unwrap();
        assert_eq!(j["enabled"], true);
        assert_eq!(j["items"][0]["n"], 2);
        assert_eq!(j["items"][0]["created"], "2026-06-25T10:00:00+00:00");
    }

    #[test]
    fn restore_report_dto_serializes() {
        let dto = RestoreReportDto {
            n: 1,
            files: vec!["a.md".into()],
            new_checkpoint: Some(3),
        };
        let j = serde_json::to_value(&dto).unwrap();
        assert_eq!(j["n"], 1);
        assert_eq!(j["new_checkpoint"], 3);
    }
}
