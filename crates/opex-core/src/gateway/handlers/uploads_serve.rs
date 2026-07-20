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
        // Prefer the stored original filename (fix: downloads used to land as
        // the row UUID because the header hard-coded `filename="{id}"`). Fall
        // back to the UUID when the upload has no client-side name (tool_output
        // binaries, agent icons).
        let id_fallback = id.to_string();
        let display_name = row
            .filename
            .as_deref()
            .and_then(sanitize_filename_for_disposition)
            .unwrap_or(&id_fallback);
        // Modern browsers prefer the percent-encoded `filename*` form for
        // non-ASCII (RFC 6266 / RFC 5987). Emit both: legacy `filename="..."`
        // carries an ASCII fallback, `filename*=UTF-8''...` carries the real
        // name including Cyrillic / CJK.
        let ascii_fallback = display_name.chars().map(|c| if c.is_ascii() { c } else { '_' }).collect::<String>();
        let disposition = format!(
            "attachment; filename=\"{}\"; filename*=UTF-8''{}",
            ascii_fallback.replace('"', ""),
            percent_encode_filename(display_name),
        );
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

/// Strip path components and reject empty / control-char / overlong names.
/// Returns `None` when the input has no usable display value, in which case
/// the caller falls back to the row UUID.
///
/// The input is the raw client-supplied filename. Even though core also
/// strips path components at upload time (media.rs), this is defense-in-depth
/// against callers that write directly to the column.
fn sanitize_filename_for_disposition(name: &str) -> Option<&str> {
    // Strip directory components — only the final segment is the filename.
    let leaf = name.split(['/', '\\']).next_back()?;
    let trimmed = leaf.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    // Reject control chars (C0 + DEL) — they cannot legally appear in an HTTP
    // header value and would force the whole Content-Disposition to drop.
    if trimmed.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    // Cap length — filenames over 255 chars break some filesystems on save.
    if trimmed.chars().count() > 255 {
        return None;
    }
    Some(trimmed)
}

/// Percent-encode a filename for the `filename*=UTF-8''<encoded>` form of
/// Content-Disposition (RFC 6266 / RFC 5987). Reserved chars per RFC 3986 +
/// the HTTP-header context (quote, backslash) are encoded; spaces too.
fn percent_encode_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len() * 3);
    for byte in name.as_bytes() {
        // RFC 5987 attr-char = ALPHA / DIGIT / "!" / "#" / "$" / "&" / "+" /
        // "-" / "." / "^" / "_" / "`" / "|" / "~"
        let safe = byte.is_ascii_alphanumeric()
            || matches!(byte, b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~');
        if safe {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_simple_filename() {
        assert_eq!(
            sanitize_filename_for_disposition("chroma_api.json"),
            Some("chroma_api.json")
        );
    }

    #[test]
    fn sanitize_strips_path_components() {
        // Defense-in-depth: callers also strip paths at upload, but a direct
        // DB write could otherwise leak a traversal attempt into the header.
        assert_eq!(
            sanitize_filename_for_disposition("/etc/passwd"),
            Some("passwd")
        );
        assert_eq!(
            sanitize_filename_for_disposition(r"C:\Users\me\photo.png"),
            Some("photo.png")
        );
        assert_eq!(
            sanitize_filename_for_disposition("../evil.md"),
            Some("evil.md")
        );
    }

    #[test]
    fn sanitize_rejects_empty_and_traversal_only() {
        assert_eq!(sanitize_filename_for_disposition(""), None);
        assert_eq!(sanitize_filename_for_disposition("   "), None);
        assert_eq!(sanitize_filename_for_disposition("."), None);
        assert_eq!(sanitize_filename_for_disposition(".."), None);
        assert_eq!(sanitize_filename_for_disposition("/"), None);
        assert_eq!(sanitize_filename_for_disposition("/.."), None);
    }

    #[test]
    fn sanitize_rejects_control_chars() {
        // CR/LF would break HTTP header framing — must drop the whole header
        // (caller falls back to UUID) rather than emit a malformed one.
        assert_eq!(sanitize_filename_for_disposition("evil\r\nSet-Cookie: x"), None);
        assert_eq!(sanitize_filename_for_disposition("tab\there"), None);
        assert_eq!(sanitize_filename_for_disposition("null\0byte"), None);
    }

    #[test]
    fn sanitize_rejects_overlong() {
        let huge = "a".repeat(256);
        assert_eq!(sanitize_filename_for_disposition(&huge), None);
        let max = "a".repeat(255);
        assert_eq!(sanitize_filename_for_disposition(&max), Some(max.as_str()));
    }

    #[test]
    fn sanitize_keeps_cyrillic_and_unicode_letters() {
        // Non-ASCII letters are valid — they'll go through the `filename*`
        // percent-encoded arm. The ASCII fallback replaces them with `_`.
        assert_eq!(sanitize_filename_for_disposition("Конспект.md"), Some("Конспект.md"));
        assert_eq!(sanitize_filename_for_disposition("日本語.txt"), Some("日本語.txt"));
    }

    #[test]
    fn percent_encode_replaces_reserved_and_non_ascii() {
        // RFC 5987 attr-char set + space + non-ASCII → percent-encoded.
        assert_eq!(percent_encode_filename("simple.json"), "simple.json");
        assert_eq!(percent_encode_filename("with space.txt"), "with%20space.txt");
        assert_eq!(percent_encode_filename("quote\"name"), "quote%22name");
        // Cyrillic 'К' = D0 9A in UTF-8.
        assert_eq!(percent_encode_filename("К"), "%D0%9A");
        // Hyphen / dot / underscore / tilde stay literal.
        assert_eq!(percent_encode_filename("a-b.c_d~e"), "a-b.c_d~e");
    }
}
