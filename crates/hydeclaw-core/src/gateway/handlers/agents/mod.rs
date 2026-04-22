mod crud;
pub mod dto;
mod lifecycle;
mod schema;

pub(crate) use crud::*;
pub use lifecycle::start_agent_from_config;
#[allow(unused_imports)]
pub(crate) use schema::{validate_agent_name, agent_config_path};

use axum::{
    Router,
    routing::{get, post, delete},
};

use super::super::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents", get(api_agents).post(api_create_agent))
        .route("/api/agents/{name}", get(api_get_agent).put(api_update_agent).delete(api_delete_agent))
        .route("/api/agents/{name}/tasks", get(api_agent_tasks))
        .route("/api/agents/{name}/model-override", post(super::chat::set_model_override))
        .route("/api/approvals", get(api_list_approvals))
        .route("/api/approvals/{id}/resolve", post(api_resolve_approval))
        .route("/api/approvals/allowlist", get(api_list_allowlist).post(api_add_to_allowlist))
        .route("/api/approvals/allowlist/{id}", delete(api_delete_from_allowlist))
}
