//! `PUT/DELETE /api/agents/{name}/icon` — multipart upload + delete for agent icons.
//!
//! The route is not yet merged into the main router; this commit is a bridge
//! landing the handler ahead of the wiring task in the uploads-to-db migration
//! plan. `#[allow(dead_code)]` mirrors the precedent set by Tasks 3 and 4.

#![allow(dead_code)]

use axum::{
    Json, Router,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::put,
};
use serde::Serialize;

use crate::gateway::clusters::{AgentCore, AuthServices, ConfigServices, InfraServices};
use crate::gateway::state::AppState;
use crate::uploads::{HISTORICAL_URL_TTL_SECS, mint_uploads_url};

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/agents/{name}/icon",
        put(api_put_agent_icon).delete(api_delete_agent_icon),
    )
}

const ALLOWED_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
const MAX_BYTES: usize = 10 * 1024 * 1024; // 10 MB

#[derive(Debug, Serialize)]
struct IconResponse {
    icon_url: String,
}

/// Build the public base URL for signed URLs, mirroring `media.rs:87-92`.
fn public_base(cfg: &ConfigServices) -> String {
    if let Some(ref pu) = cfg.config.gateway.public_url {
        pu.trim_end_matches('/').to_string()
    } else {
        let port = cfg
            .config
            .gateway
            .listen
            .rsplit(':')
            .next()
            .unwrap_or("18789");
        format!("http://localhost:{port}")
    }
}

pub(crate) async fn api_put_agent_icon(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(cfg): State<ConfigServices>,
    Path(name): Path<String>,
    mut multipart: Multipart,
) -> axum::response::Response {
    let known_agents = agents.agent_names().await;
    if !known_agents.iter().any(|n| n == &name) {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }

    let mut data: Option<Vec<u8>> = None;
    let mut mime: Option<String> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() != Some("image") {
            continue;
        }
        mime = field.content_type().map(|s| s.to_string());
        match field.bytes().await {
            Ok(bytes) if bytes.len() <= MAX_BYTES => data = Some(bytes.to_vec()),
            Ok(bytes) => {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "icon must be <= {} bytes, got {}",
                        MAX_BYTES,
                        bytes.len()
                    ),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("multipart read failed: {e}"),
                )
                    .into_response();
            }
        }
        break;
    }

    let data = match data {
        Some(d) if !d.is_empty() => d,
        _ => return (StatusCode::BAD_REQUEST, "missing 'image' field").into_response(),
    };
    let mime = mime.unwrap_or_else(|| "application/octet-stream".to_string());
    if !ALLOWED_MIME.contains(&mime.as_str()) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("MIME {mime} not allowed; expected one of {ALLOWED_MIME:?}"),
        )
            .into_response();
    }

    let id = match crate::db::uploads::upsert_agent_icon(&infra.db, &name, &mime, &data).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, agent = %name, "icon upsert failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let key = auth.secrets.get_upload_hmac_key();
    let base = public_base(&cfg);
    let icon_url = mint_uploads_url(&base, id, &key, HISTORICAL_URL_TTL_SECS);

    (StatusCode::OK, Json(IconResponse { icon_url })).into_response()
}

pub(crate) async fn api_delete_agent_icon(
    State(infra): State<InfraServices>,
    Path(name): Path<String>,
) -> axum::response::Response {
    match crate::db::uploads::delete_agent_icon(&infra.db, &name).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, agent = %name, "icon delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
