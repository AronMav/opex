//! `GET /api/internal/youtube-cookies` — resolve YouTube cookies (Netscape
//! cookies.txt format) from the secrets vault, for the `toolgate`
//! `video_helpers.py` yt-dlp download flow.
//!
//! **Authentication:** standard Bearer-token auth middleware — this endpoint
//! is NOT in `PUBLIC_EXACT` / `PUBLIC_PREFIX` / `LOOPBACK_EXACT` in
//! `gateway/middleware.rs`, so an auth header is required for every caller,
//! including loopback (same shape as `internal_creds.rs`). `toolgate` already
//! holds `OPEX_AUTH_TOKEN` for this kind of call-back into core.
//!
//! Secret `YOUTUBE_COOKIES` is stored as the raw Netscape cookies file content
//! (multi-line text) under the global scope (`""`) in the vault. Returns 404
//! if unset. The secret value is never logged.
//!
//! This is a direct clone of the `internal_creds.rs` pattern (ITS_CREDENTIALS):
//! store a credential blob in the vault, expose it via a loopback-authenticated
//! endpoint that toolgate fetches with its existing `OPEX_AUTH_TOKEN`.

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::gateway::clusters::AuthServices;
use crate::gateway::state::AppState;

/// Secret name for YouTube cookies in the vault.
pub(crate) const YOUTUBE_COOKIES_SECRET: &str = "YOUTUBE_COOKIES";

/// Response body for `GET /api/internal/youtube-cookies`.
#[derive(Debug, Serialize)]
pub(crate) struct YoutubeCookiesResponse {
    /// Raw Netscape cookies file content.
    pub cookies: String,
}

/// Handler: return the YouTube cookies content from the vault.
async fn get_youtube_cookies(State(auth): State<AuthServices>) -> impl IntoResponse {
    // Global scope only ("") — no per-agent scoping. Cookies are shared across
    // all agents (same YouTube account / datacenter IP).
    match auth.secrets.get_strict(YOUTUBE_COOKIES_SECRET).await {
        Some(cookies) => {
            // Basic sanity: non-empty after trimming.
            if cookies.trim().is_empty() {
                return (
                    StatusCode::NOT_FOUND,
                    "YOUTUBE_COOKIES is set but empty",
                )
                    .into_response();
            }
            // no-store: cookies contain session tokens — never cache.
            (
                StatusCode::OK,
                [(header::CACHE_CONTROL, "no-store")],
                Json(YoutubeCookiesResponse { cookies }),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "YOUTUBE_COOKIES not set").into_response(),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/internal/youtube-cookies", get(get_youtube_cookies))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_name_is_valid() {
        // The secrets API validates name as [A-Za-z0-9_] — YOUTUBE_COOKIES
        // must pass this check.
        assert!(
            YOUTUBE_COOKIES_SECRET
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "secret name must be alphanumeric + underscore only"
        );
        assert!(YOUTUBE_COOKIES_SECRET.len() <= 128);
    }

    #[test]
    fn response_serializes_cookies_field() {
        let resp = YoutubeCookiesResponse {
            cookies: "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t9999\tsid\tabc\n".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"cookies\""));
        assert!(json.contains("youtube.com"));
    }

    #[test]
    fn response_with_empty_cookies_serializes() {
        let resp = YoutubeCookiesResponse {
            cookies: "".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"cookies\":\"\""));
    }
}