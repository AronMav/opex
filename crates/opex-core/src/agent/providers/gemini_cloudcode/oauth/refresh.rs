//! Refresh-token rotation: `get_valid_access_token` is the single entry point
//! consumed by Module 3's `GeminiCloudCodeProvider`.
//!
//! Security: this file must NOT log `access_token` or `refresh_token` values.
//! Use `crate::redact::redact_oauth_str` on any string before tracing.

use super::{
    client_creds::resolve_client_creds,
    storage::{_save_to_path, load_credentials, with_credentials_lock},
    types::{GoogleCredentials, OauthError, RefreshParts},
    REFRESH_SKEW_SECONDS, TOKEN_ENDPOINT,
};
use crate::redact::redact_oauth_str;

// ── Expiry check ──────────────────────────────────────────────────────────────

/// Return `true` when the access token is within `REFRESH_SKEW_SECONDS` of expiry.
pub(crate) fn is_near_expiry(expires_ms: i64) -> bool {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let skew_ms = REFRESH_SKEW_SECONDS * 1_000;
    expires_ms - now_ms < skew_ms
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Get a valid access token, refreshing if necessary.
///
/// Delegates to `get_valid_access_token_at` with the canonical `TOKEN_ENDPOINT`.
///
/// **Test seam:** set `OPEX_GEMINI_TEST_ACCESS_TOKEN` to bypass the entire
/// OAuth flow and return a synthetic token. This is guarded by `#[cfg(test)]`
/// so it has zero production effect.
///
/// **Test endpoint override:** set `OPEX_GEMINI_TEST_TOKEN_ENDPOINT` to
/// redirect refresh calls to a wiremock server. Works at runtime (no cfg guard)
/// so integration tests can exercise the real refresh path against a mock.
pub async fn get_valid_access_token(force_refresh: bool) -> Result<String, OauthError> {
    #[cfg(test)]
    {
        if let Ok(tok) = std::env::var("OPEX_GEMINI_TEST_ACCESS_TOKEN")
            && !tok.is_empty()
        {
            return Ok(tok);
        }
    }
    let endpoint = std::env::var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT")
        .unwrap_or_else(|_| TOKEN_ENDPOINT.to_string());
    get_valid_access_token_at(force_refresh, &endpoint).await
}

/// Internal variant that accepts an explicit token endpoint URL.
///
/// Used by tests to point at a wiremock server without changing the compile-time
/// constant (`TOKEN_ENDPOINT`).
///
/// # Flow
///
/// 1. Acquire the cross-process storage lock; load credentials from disk.
///    Returns `OauthError::NotAuthenticated` if no credentials file exists.
/// 2. If `force_refresh` is false and the token is not near expiry, return the
///    cached access token immediately (no network call).
/// 3. Otherwise POST `token_endpoint` with the refresh grant.
/// 4. On HTTP 400 `invalid_grant`: wipe credentials, return `ReAuthRequired`.
/// 5. On success: persist updated credentials under lock; return new access token.
///
/// Logs `tracing::info!` with email + expires_in_s on each refresh — never the
/// token itself.
pub(crate) async fn get_valid_access_token_at(
    force_refresh: bool,
    token_endpoint: &str,
) -> Result<String, OauthError> {
    // Pass 1: load and decide under lock (sync, no async in closure).
    let creds = with_credentials_lock(|| {
        load_credentials().ok_or(OauthError::NotAuthenticated)
    })?;

    if !force_refresh && !is_near_expiry(creds.expires_ms) {
        return Ok(creds.access.clone());
    }

    // Pass 2: perform the refresh (async, lock NOT held — async closures are
    // unstable on stable Rust).
    let parts = creds.refresh_parts();
    let refresh_token = parts.refresh_token.clone();

    let client_creds = resolve_client_creds();

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(OauthError::Http)?;

    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client_creds.client_id.as_str()),
        ("client_secret", client_creds.client_secret.as_str()),
    ];

    let resp = client
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(OauthError::Http)?;

    let status = resp.status();
    let body = resp.text().await.map_err(OauthError::Http)?;

    if status == reqwest::StatusCode::BAD_REQUEST {
        // HTTP 400 with invalid_grant → the refresh token is dead.
        if body.contains("invalid_grant") {
            tracing::warn!("Google OAuth refresh token revoked — clearing credentials");
            super::storage::clear_credentials();
            return Err(OauthError::ReAuthRequired);
        }
        return Err(OauthError::TokenExchange {
            status: status.as_u16(),
            body: redact_oauth_str(&body),
        });
    }

    if !status.is_success() {
        return Err(OauthError::TokenExchange {
            status: status.as_u16(),
            body: redact_oauth_str(&body),
        });
    }

    let json: serde_json::Value = serde_json::from_str(&body).map_err(OauthError::Json)?;

    let new_access = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OauthError::TokenExchange {
            status: status.as_u16(),
            body: "missing access_token in response".to_string(),
        })?
        .to_string();

    let expires_in = json
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let new_expires_ms = chrono::Utc::now().timestamp_millis() + expires_in * 1_000;

    // Pass 3: persist under lock, re-packing to preserve project IDs.
    // Use `_save_to_path` (no inner lock) inside `with_credentials_lock` to
    // avoid a double-lock: `save_credentials` would acquire the same lock again
    // and immediately timeout on platforms using a non-reentrant cross-process
    // flock.
    let new_refresh_packed = RefreshParts {
        refresh_token: refresh_token.clone(),
        project_id: parts.project_id.clone(),
        managed_project_id: parts.managed_project_id.clone(),
    }
    .pack();

    let updated = GoogleCredentials {
        refresh: new_refresh_packed,
        access: new_access.clone(),
        expires_ms: new_expires_ms,
        email: creds.email.clone(),
    };

    let creds_path = super::storage::credentials_path();
    with_credentials_lock(|| _save_to_path(&updated, &creds_path))?;

    tracing::info!(
        email = %creds.email,
        expires_in_s = expires_in,
        "refreshed Google access token"
    );

    Ok(new_access)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::providers::gemini_cloudcode::oauth::{
        storage::_save_to_path,
        types::{GoogleCredentials, OauthError, RefreshParts},
    };
    use serial_test::serial;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Env var that overrides the credentials file path (read first by the dual-read helper in `storage::credentials_path`).
    const CREDS_PATH_ENV: &str = "OPEX_OAUTH_CREDENTIALS_PATH";

    /// Set `OPEX_OAUTH_CREDENTIALS_PATH` to a fresh temp path, run `f`, restore.
    ///
    /// The returned `TempDir` keeps the dir alive for the duration of the test.
    fn set_tmp_creds_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let creds_path = dir.path().join("google_oauth.json");
        unsafe {
            std::env::set_var(CREDS_PATH_ENV, &creds_path);
        }
        (dir, creds_path)
    }

    /// Restore or remove the env var.
    fn restore_creds_path(prev: Option<String>) {
        unsafe {
            match prev {
                Some(v) => std::env::set_var(CREDS_PATH_ENV, v),
                None => std::env::remove_var(CREDS_PATH_ENV),
            }
        }
    }

    fn near_expiry_creds(refresh: &str) -> GoogleCredentials {
        GoogleCredentials {
            refresh: RefreshParts {
                refresh_token: refresh.to_string(),
                project_id: String::new(),
                managed_project_id: String::new(),
            }
            .pack(),
            access: "old_access".to_string(),
            expires_ms: chrono::Utc::now().timestamp_millis() + 10_000, // 10s < REFRESH_SKEW=60s
            email: "test@example.com".to_string(),
        }
    }

    // ── Unit tests (no network) ───────────────────────────────────────────────

    #[tokio::test]
    async fn skips_refresh_when_fresh() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let far_future = now_ms + 10 * 60 * 1000; // 10 min from now
        assert!(!is_near_expiry(far_future), "10 min ahead should not be near expiry");
    }

    #[tokio::test]
    async fn refreshes_when_near_expiry() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let near = now_ms + 30 * 1000; // 30 sec from now (< REFRESH_SKEW_SECONDS=60)
        assert!(is_near_expiry(near), "30 sec ahead should be near expiry");
    }

    // ── Wiremock tests ────────────────────────────────────────────────────────

    /// HTTP 400 `invalid_grant` → credentials wiped, `ReAuthRequired` returned.
    #[tokio::test]
    #[serial(oauth_creds_path)]
    async fn invalid_grant_clears_credentials() {
        let prev = std::env::var(CREDS_PATH_ENV).ok();
        let (_dir, cred_path) = set_tmp_creds_path();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(
                    r#"{"error":"invalid_grant","error_description":"Token has been revoked."}"#,
                ),
            )
            .mount(&server)
            .await;

        _save_to_path(&near_expiry_creds("dead_refresh_token"), &cred_path).unwrap();

        let result =
            get_valid_access_token_at(false, &format!("{}/token", server.uri())).await;

        restore_creds_path(prev);

        assert!(
            matches!(result, Err(OauthError::ReAuthRequired)),
            "invalid_grant must return ReAuthRequired: {result:?}"
        );
        // load_credentials() now resolves the *restored* (original) path, so
        // check it against the temp path directly.
        assert!(
            !cred_path.exists(),
            "credentials file must be deleted after invalid_grant"
        );
    }

    /// Successful refresh returns new access token.
    #[tokio::test]
    #[serial(oauth_creds_path)]
    async fn refreshes_when_near_expiry_calls_token_endpoint() {
        let prev = std::env::var(CREDS_PATH_ENV).ok();
        let (_dir, cred_path) = set_tmp_creds_path();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "new_access_token_value",
                    "expires_in": 3600,
                    "token_type": "Bearer"
                })),
            )
            .mount(&server)
            .await;

        _save_to_path(&near_expiry_creds("valid_refresh"), &cred_path).unwrap();

        let result =
            get_valid_access_token_at(false, &format!("{}/token", server.uri())).await;

        restore_creds_path(prev);

        assert!(result.is_ok(), "refresh must succeed: {result:?}");
        assert_eq!(result.unwrap(), "new_access_token_value");
    }
}
