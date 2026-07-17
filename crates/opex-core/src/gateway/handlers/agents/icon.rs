//! `PUT/DELETE /api/agents/{name}/icon` — multipart upload + delete for agent icons.
//! `POST /api/agents/{name}/icon/json` — JSON `{mime, data_base64}` for agent self-service.

use axum::{
    Json, Router,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{post, put},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};

use crate::gateway::clusters::{AgentCore, AuthServices, InfraServices};
use crate::gateway::state::AppState;
use crate::uploads::{HISTORICAL_URL_TTL_SECS, mint_uploads_url};

pub(crate) fn routes() -> Router<AppState> {
    // Axum's default per-request body limit is 2 MiB. Multipart and JSON
    // routes need different caps so the handler's MAX_BYTES check is
    // reachable in both: the multipart body is roughly equal to the binary
    // payload, but the JSON body carries base64 (4/3 inflation) plus a
    // small envelope. Split the sub-routers so each can declare its own
    // limit.
    let multipart = Router::new()
        .route(
            "/api/agents/{name}/icon",
            put(api_put_agent_icon),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BYTES));
    let json = Router::new()
        .route("/api/agents/{name}/icon/json", post(api_post_agent_icon_json))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_JSON_BODY_BYTES));
    multipart.merge(json)
}

const ALLOWED_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
const MAX_BYTES: usize = 10 * 1024 * 1024; // 10 MB binary cap (handler-level)

/// Body-cap for the JSON variant: MAX_BYTES of binary, base64-encoded
/// (4/3 inflation), plus 4 KiB slack for the JSON envelope (`mime`,
/// quoting, keys). Mirrors the handler-level MAX_BYTES so a 10 MiB icon
/// reaches `store_icon` and surfaces the explicit "icon must be <= N
/// bytes" error instead of axum's generic 413.
const MAX_JSON_BODY_BYTES: usize = MAX_BYTES.div_ceil(3) * 4 + 4096;

#[derive(Debug, Serialize)]
struct IconResponse {
    icon_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IconJsonPayload {
    mime: String,
    data_base64: String,
}

/// Shared validation + upsert + URL mint. Both PUT (multipart) and POST/json
/// route into this so they cannot drift apart.
async fn store_icon(
    infra: &InfraServices,
    auth: &AuthServices,
    agent_name: &str,
    mime: &str,
    data: &[u8],
) -> axum::response::Response {
    if data.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty image payload").into_response();
    }
    if data.len() > MAX_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("icon must be <= {} bytes, got {}", MAX_BYTES, data.len()),
        )
            .into_response();
    }
    if !ALLOWED_MIME.contains(&mime) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("MIME {mime} not allowed; expected one of {ALLOWED_MIME:?}"),
        )
            .into_response();
    }

    let id = match crate::db::uploads::upsert_agent_icon(&infra.db, agent_name, mime, data).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, agent = %agent_name, "icon upsert failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Root-relative URL — rendered in the same-origin web UI, so it must not
    // depend on gateway.public_url (see crate::uploads::web_uploads_base()).
    let key = auth.secrets.get_upload_hmac_key();
    let icon_url = mint_uploads_url(
        crate::uploads::web_uploads_base(),
        id,
        &key,
        HISTORICAL_URL_TTL_SECS,
    );
    (StatusCode::OK, Json(IconResponse { icon_url })).into_response()
}

pub(crate) async fn api_put_agent_icon(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
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
            Ok(bytes) => data = Some(bytes.to_vec()),
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
        Some(d) => d,
        None => return (StatusCode::BAD_REQUEST, "missing 'image' field").into_response(),
    };
    let mime = mime.unwrap_or_else(|| "application/octet-stream".to_string());
    store_icon(&infra, &auth, &name, &mime, &data).await
}

/// JSON variant for agent self-service: `{mime, data_base64}`.
/// Mirrors the multipart endpoint's validation. Agents reach this through the
/// `set_my_icon` YAML tool.
pub(crate) async fn api_post_agent_icon_json(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(name): Path<String>,
    Json(payload): Json<IconJsonPayload>,
) -> axum::response::Response {
    let known_agents = agents.agent_names().await;
    if !known_agents.iter().any(|n| n == &name) {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }

    let data = match B64.decode(payload.data_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("data_base64 decode failed: {e}"),
            )
                .into_response();
        }
    };
    store_icon(&infra, &auth, &name, &payload.mime, &data).await
}

