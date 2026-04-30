use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, delete},
};
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::{AuthServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/access/{agent}/pending", get(api_access_pending))
        .route("/api/access/{agent}/approve/{code}", post(api_access_approve))
        .route("/api/access/{agent}/reject/{code}", post(api_access_reject))
        .route("/api/access/{agent}/users", get(api_access_list_users))
        .route("/api/access/{agent}/users/{user_id}", delete(api_access_remove_user))
}

pub(crate) async fn api_access_pending(
    State(auth): State<AuthServices>,
    axum::extract::Path(agent): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(guard) = auth.access_guards.read().await.get(&agent).cloned() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "access control not configured for this agent"}))).into_response();
    };
    let pairings = guard.pending_pairings_list().await;
    Json(json!({ "pending": pairings })).into_response()
}

pub(crate) async fn api_access_approve(
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    axum::extract::Path((agent, code)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let Some(guard) = auth.access_guards.read().await.get(&agent).cloned() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "access control not configured for this agent"}))).into_response();
    };
    let approver = guard.owner_id.as_deref().unwrap_or("ui");
    let (success, info) = guard.approve_pairing(&code, approver).await;
    if success {
        crate::db::audit::audit_spawn(infra.db.clone(), agent.clone(), crate::db::audit::event_types::ACCESS_APPROVED, Some(approver.to_string()), json!({"agent": agent, "user": info}));
        Json(json!({"ok": true, "user": info})).into_response()
    } else {
        (StatusCode::BAD_REQUEST, Json(json!({"error": info}))).into_response()
    }
}

pub(crate) async fn api_access_reject(
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    axum::extract::Path((agent, code)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let Some(guard) = auth.access_guards.read().await.get(&agent).cloned() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "access control not configured for this agent"}))).into_response();
    };
    let removed = guard.reject_pairing(&code).await;
    if removed {
        crate::db::audit::audit_spawn(infra.db.clone(), agent.clone(), crate::db::audit::event_types::ACCESS_REJECTED, None, json!({"agent": agent, "code": code}));
    }
    Json(json!({"ok": removed})).into_response()
}

pub(crate) async fn api_access_list_users(
    State(auth): State<AuthServices>,
    axum::extract::Path(agent): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(guard) = auth.access_guards.read().await.get(&agent).cloned() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "access control not configured for this agent"}))).into_response();
    };
    match crate::db::access::list_allowed_users(&guard.db, &agent).await {
        Ok(users) => {
            let list: Vec<Value> = users.iter().map(|u| json!({
                "channel_user_id": u.channel_user_id,
                "display_name": u.display_name,
                "approved_at": u.approved_at.to_rfc3339(),
            })).collect();
            Json(json!({ "users": list })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_access_remove_user(
    State(auth): State<AuthServices>,
    axum::extract::Path((agent, user_id)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let Some(guard) = auth.access_guards.read().await.get(&agent).cloned() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "access control not configured for this agent"}))).into_response();
    };
    match crate::db::access::remove_allowed_user(&guard.db, &agent, &user_id).await {
        Ok(deleted) => Json(json!({"ok": deleted})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}
