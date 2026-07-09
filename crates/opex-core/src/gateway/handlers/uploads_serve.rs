//! `GET /api/uploads/{id}` — read-through to the `uploads` table with HMAC verification.
//!
//! This endpoint is excluded from the bearer auth middleware
//! (see `crate::gateway::middleware::PUBLIC_PREFIX`) so HTML `img`/`audio` tags
//! work without bearer headers. Security comes from the HMAC-signed query
//! string (`?sig=&exp=`).

use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::gateway::clusters::{AuthServices, InfraServices};
use crate::gateway::state::AppState;
use crate::uploads::verify_uploads_url;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/uploads/{id}", get(api_uploads_serve))
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadsQuery {
    pub sig: String,
    pub exp: u64,
}

pub(crate) async fn api_uploads_serve(
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    Path(id_str): Path<String>,
    Query(q): Query<UploadsQuery>,
) -> Response {
    let id = match Uuid::parse_str(&id_str) {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let key = auth.secrets.get_upload_hmac_key();

    if verify_uploads_url(id, &q.sig, q.exp, &key).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let row = match crate::db::uploads::get_by_id(&infra.db, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "uploads serve: db error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut headers = HeaderMap::new();
    if let Ok(mime) = HeaderValue::from_str(&row.mime) {
        headers.insert(header::CONTENT_TYPE, mime);
    }
    if let Ok(len) = HeaderValue::from_str(&row.size_bytes.to_string()) {
        headers.insert(header::CONTENT_LENGTH, len);
    }
    let etag = format!("\"{}\"", hex::encode(&row.sha256));
    if let Ok(etag_hv) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, etag_hv);
    }
    if let Ok(cc) = HeaderValue::from_str("public, max-age=3600, immutable") {
        headers.insert(header::CACHE_CONTROL, cc);
    }

    // XSS hardening: always disable MIME sniffing, and force non-inlineable
    // bytes (anything other than image/audio/video — and explicitly excluding
    // image/svg+xml, which can carry script) to download as an attachment so
    // they cannot execute same-origin.
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if !is_inlineable_mime(&row.mime) {
        let disposition = format!("attachment; filename=\"{id}\"");
        if let Ok(hv) = HeaderValue::from_str(&disposition) {
            headers.insert(header::CONTENT_DISPOSITION, hv);
        }
    }

    (StatusCode::OK, headers, row.data).into_response()
}

/// Returns true for MIME types safe to render inline in the browser without
/// risking script execution: `image/*` (except `image/svg+xml`), `audio/*`,
/// and `video/*`. Everything else (html, svg, pdf, text/*, application/*) is
/// forced to download via `Content-Disposition: attachment`.
pub(crate) fn is_inlineable_mime(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    if lower.starts_with("image/") {
        // Exclude image/svg+xml — SVG can carry <script>.
        !lower.starts_with("image/svg")
    } else {
        lower.starts_with("audio/") || lower.starts_with("video/")
    }
}
