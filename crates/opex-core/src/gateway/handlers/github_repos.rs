use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, delete},
    Json,
};
use crate::gateway::clusters::InfraServices;
use crate::gateway::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{name}/github/repos", get(api_list_github_repos).post(api_add_github_repo))
        .route("/api/agents/{name}/github/repos/{id}", delete(api_delete_github_repo))
}

#[derive(serde::Deserialize)]
pub(crate) struct AddRepoRequest {
    pub owner: String,
    pub repo: String,
}

pub(crate) async fn api_list_github_repos(
    State(infra): State<InfraServices>,
    Path(agent_name): Path<String>,
) -> impl IntoResponse {
    match crate::db::github::list_repos(&infra.db, &agent_name).await {
        Ok(repos) => Json(serde_json::json!({"repos": repos})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(crate) async fn api_add_github_repo(
    State(infra): State<InfraServices>,
    Path(agent_name): Path<String>,
    Json(body): Json<AddRepoRequest>,
) -> impl IntoResponse {
    if body.owner.is_empty() || body.repo.is_empty() {
        return (StatusCode::BAD_REQUEST, "owner and repo are required").into_response();
    }
    match crate::db::github::add_repo(&infra.db, &agent_name, &body.owner, &body.repo).await {
        Ok(repo) => (StatusCode::CREATED, Json(repo)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(crate) async fn api_delete_github_repo(
    State(infra): State<InfraServices>,
    Path((agent_name, id)): Path<(String, uuid::Uuid)>,
) -> impl IntoResponse {
    match crate::db::github::remove_repo(&infra.db, id, &agent_name).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
