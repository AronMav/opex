use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Redirect},
    routing::{get, post, delete},
};
use serde::Deserialize;
use std::collections::HashMap;
use crate::gateway::AppState;
use crate::gateway::ApiError;
use crate::gateway::clusters::{AuthServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/oauth/callback", get(api_oauth_callback))
        .route("/api/oauth/accounts", get(api_oauth_accounts_list).post(api_oauth_account_create))
        .route("/api/oauth/accounts/{id}", delete(api_oauth_account_delete))
        .route("/api/oauth/accounts/{id}/connect", post(api_oauth_account_connect))
        .route("/api/oauth/accounts/{id}/revoke", post(api_oauth_account_revoke))
        .route("/api/agents/{name}/oauth/bindings", get(api_oauth_bindings_list).post(api_oauth_binding_create))
        .route("/api/agents/{name}/oauth/bindings/{provider}", delete(api_oauth_binding_delete))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_uuid(id: &str) -> Result<sqlx::types::Uuid, impl IntoResponse> {
    id.parse::<sqlx::types::Uuid>()
        .map_err(|_| ApiError::BadRequest("invalid UUID".to_string()))
}

/// Restart sandbox containers for agents affected by an OAuth change.
async fn restart_agent_sandboxes(infra: &InfraServices, auth: &AuthServices, agent_ids: &[String]) {
    let Some(ref sandbox) = infra.sandbox else { return };
    let workspace_dir = match tokio::fs::canonicalize(crate::config::WORKSPACE_DIR).await {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return,
    };
    for agent_id in agent_ids {
        if let Err(e) = sandbox.restart_container(agent_id, &workspace_dir, false, Some(&auth.oauth)).await {
            tracing::warn!(agent = %agent_id, error = %e, "failed to restart sandbox after OAuth change");
        }
    }
}

/// Spawn sandbox restart in background for given agents.
fn spawn_sandbox_restart(infra: InfraServices, auth: AuthServices, agents: Vec<String>) {
    // AUDIT-FF-013: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
    tokio::spawn(async move { restart_agent_sandboxes(&infra, &auth, &agents).await; });
}

/// Find all agents bound to a specific OAuth account.
async fn agents_bound_to_account(infra: &InfraServices, account_id: sqlx::types::Uuid) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT agent_id FROM agent_oauth_bindings WHERE account_id = $1"
    )
    .bind(account_id)
    .fetch_all(&infra.db)
    .await
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Account CRUD
// ---------------------------------------------------------------------------

/// GET /api/oauth/accounts?provider=github
pub(crate) async fn api_oauth_accounts_list(
    State(auth): State<AuthServices>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let provider = params.get("provider").map(String::as_str);
    match auth.oauth.list_accounts(provider).await {
        Ok(accounts) => Json(serde_json::json!({ "accounts": accounts })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct CreateAccountBody {
    pub provider: String,
    pub display_name: String,
    pub client_id: String,
    pub client_secret: String,
}

/// POST /api/oauth/accounts
pub(crate) async fn api_oauth_account_create(
    State(auth): State<AuthServices>,
    Json(body): Json<CreateAccountBody>,
) -> impl IntoResponse {
    match auth
        .oauth
        .create_account(&body.provider, &body.display_name, &body.client_id, &body.client_secret)
        .await
    {
        Ok(id) => Json(serde_json::json!({ "ok": true, "id": id })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// DELETE /api/oauth/accounts/{id}
pub(crate) async fn api_oauth_account_delete(
    State(auth): State<AuthServices>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let account_id = match parse_uuid(&id) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match auth.oauth.delete_account(account_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/oauth/accounts/{id}/connect?agent=main
pub(crate) async fn api_oauth_account_connect(
    State(auth): State<AuthServices>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let account_id = match parse_uuid(&id) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let agent_id = params.get("agent").cloned().unwrap_or_else(|| "main".into());
    match auth.oauth.init_flow(account_id, &agent_id).await {
        Ok(url) => Json(serde_json::json!({ "auth_url": url })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// POST /api/oauth/accounts/{id}/revoke
pub(crate) async fn api_oauth_account_revoke(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let account_id = match parse_uuid(&id) {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match auth.oauth.revoke(account_id).await {
        Ok(()) => {
            let agents = agents_bound_to_account(&infra, account_id).await;
            spawn_sandbox_restart(infra, auth, agents);
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Agent binding CRUD
// ---------------------------------------------------------------------------

/// GET /api/agents/{name}/oauth/bindings
pub(crate) async fn api_oauth_bindings_list(
    State(auth): State<AuthServices>,
    Path(agent_name): Path<String>,
) -> impl IntoResponse {
    match auth.oauth.list_bindings(&agent_name).await {
        Ok(bindings) => Json(serde_json::json!({ "bindings": bindings })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct CreateBindingBody {
    pub provider: String,
    pub account_id: sqlx::types::Uuid,
}

/// POST /api/agents/{name}/oauth/bindings
pub(crate) async fn api_oauth_binding_create(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(agent_name): Path<String>,
    Json(body): Json<CreateBindingBody>,
) -> impl IntoResponse {
    match auth
        .oauth
        .bind_account(&agent_name, &body.provider, body.account_id)
        .await
    {
        Ok(()) => {
            spawn_sandbox_restart(infra, auth, vec![agent_name.clone()]);
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// DELETE /api/agents/{name}/oauth/bindings/{provider}
pub(crate) async fn api_oauth_binding_delete(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path((agent_name, provider)): Path<(String, String)>,
) -> impl IntoResponse {
    match auth.oauth.unbind_account(&agent_name, &provider).await {
        Ok(()) => {
            spawn_sandbox_restart(infra, auth, vec![agent_name.clone()]);
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Backward-compatible / kept handlers
// ---------------------------------------------------------------------------

/// GET /api/oauth/providers — backward compat
pub(crate) async fn api_oauth_callback(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_token = params.get("state").cloned().unwrap_or_default();
    match auth.oauth.handle_callback(code, state_token).await {
        Ok((agent_id, provider)) => {
            spawn_sandbox_restart(infra, auth, vec![agent_id.clone()]);
            Redirect::to(&format!(
                "/integrations?connected={provider}&agent={agent_id}"
            ))
            .into_response()
        }
        Err(e) => {
            let encoded: String =
                url::form_urlencoded::byte_serialize(e.to_string().as_bytes()).collect();
            Redirect::to(&format!("/integrations?error={encoded}")).into_response()
        }
    }
}
