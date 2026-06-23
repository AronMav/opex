use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use serde::Deserialize;
use serde_json::json;

use super::super::AppState;
use crate::gateway::clusters::{AuthServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/secrets", get(list_secrets).post(set_secret))
        .route("/api/secrets/{name}", get(get_secret).delete(delete_secret))
}

pub(crate) async fn list_secrets(State(auth): State<AuthServices>) -> impl IntoResponse {
    match auth.secrets.list().await {
        Ok(secrets) => Json(json!({ "secrets": secrets })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetSecretRequest {
    name: String,
    #[serde(default)]
    value: Option<String>,
    description: Option<String>,
    /// Optional agent scope. If set, creates a per-agent secret instead of a global one.
    scope: Option<String>,
}

pub(crate) async fn set_secret(
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    Json(req): Json<SetSecretRequest>,
) -> impl IntoResponse {
    if req.name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name is required"})),
        )
            .into_response();
    }
    // Allow description-only update (no value)
    if req.value.as_ref().is_none_or(std::string::String::is_empty) && req.description.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "value or description is required"})),
        )
            .into_response();
    }
    if req.name.len() > 128
        || !req
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid name: use A-Z, a-z, 0-9, _ (max 128 chars)"})),
        )
            .into_response();
    }
    let scope = req.scope.as_deref().unwrap_or("");
    let has_value = req.value.as_ref().is_some_and(|v| !v.is_empty());

    let set_result = if has_value {
        let value = match req.value.as_deref() {
            Some(v) if !v.is_empty() => v,
            _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "value is required"}))).into_response(),
        };
        if scope.is_empty() {
            auth.secrets.set(&req.name, value, req.description.as_deref()).await
        } else {
            auth.secrets.set_scoped(&req.name, scope, value, req.description.as_deref()).await
        }
    } else {
        // Description-only update
        auth.secrets.update_description(&req.name, scope, req.description.as_deref()).await
    };
    match set_result
    {
        Ok(()) => {
            crate::db::audit::audit_spawn(infra.db.clone(), scope.to_string(), crate::db::audit::event_types::SECRET_CREATED, None, json!({"name": req.name, "scope": scope}));
            // Toolgate pulls config + provider keys on every provider call (TTL=0)
            // — no reload notification needed.
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => {
            tracing::error!(secret = %req.name, "secret set failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "failed to store secret"}))).into_response()
        }
    }
}

pub(crate) async fn get_secret(
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let scope = params.get("scope").cloned().unwrap_or_default();
    let reveal = params.get("reveal").is_some_and(|v| v == "true");

    let value = if scope.is_empty() {
        auth.secrets.get_strict(&name).await
    } else {
        auth.secrets.get_scoped(&name, &scope).await
    };

    match value {
        Some(val) => {
            let mut obj = serde_json::json!({
                "name": name,
                "masked": mask_secret_value(&val),
                "length": val.len(),
            });
            if reveal {
                obj["value"] = serde_json::json!(val);
                tracing::warn!(secret = %name, scope = %scope, "AUDIT: secret value revealed via API");
                crate::db::audit::audit_spawn(infra.db.clone(), scope.clone(), crate::db::audit::event_types::SECRET_REVEALED, None, json!({"name": name, "scope": scope}));
            }
            Json(obj).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "secret not found"})),
        )
            .into_response(),
    }
}

pub(crate) async fn delete_secret(
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let scope = params.get("scope").map_or("", std::string::String::as_str);
    match auth.secrets.delete_scoped(&name, scope).await {
        Ok(true) => {
            crate::db::audit::audit_spawn(infra.db.clone(), scope.to_string(), crate::db::audit::event_types::SECRET_DELETED, None, json!({"name": name, "scope": scope}));
            Json(json!({"ok": true})).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "secret not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(secret = %name, scope = %scope, "secret delete failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "failed to delete secret"}))).into_response()
        }
    }
}

pub(crate) fn mask_secret_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        "*".repeat(chars.len())
    } else {
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[chars.len() - 4..].iter().collect();
        format!("{prefix}...{suffix}")
    }
}
