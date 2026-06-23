//! RFC 8628 Device Authorization Grant — public D4 facade.
//!
//! This module re-exports the three public entry points consumed by Module 4
//! and adds `poll_device_flow`, a thin wrapper for callers that already hold
//! a `DeviceFlowSession` and use the default credential resolution.
//!
//! The heavy implementation lives in [`super::device_flow`]; this file is the
//! stable API boundary so Module 4 imports from `oauth::device` rather than
//! `oauth::device_flow`.

// Suppress dead_code for items wired up by Module 4.
#![allow(dead_code)]

use super::{
    client_creds::resolve_client_creds,
    device_flow::{poll_until_complete, token_endpoint},
    types::{DeviceFlowSession, GoogleCredentials, OauthError},
};

// ── Re-exports (D4 interface) ─────────────────────────────────────────────────

pub use super::device_flow::start_device_code_flow;

// ── poll_device_flow ──────────────────────────────────────────────────────────

/// Poll the token endpoint until the user authorises, the code expires, or
/// access is denied.  On success, persists credentials to storage.
///
/// Uses the 3-tier client-credential resolution chain (`resolve_client_creds`)
/// and the production `TOKEN_ENDPOINT` (overridable by
/// `OPEX_GEMINI_TEST_TOKEN_ENDPOINT` in tests).
///
/// Thin wrapper over [`device_flow::poll_until_complete`] that avoids
/// callers needing to own the client_id / client_secret strings.
pub async fn poll_device_flow(session: &DeviceFlowSession) -> Result<(), OauthError> {
    let creds = resolve_client_creds();
    poll_until_complete(
        session.clone(),
        creds.client_id,
        creds.client_secret,
        token_endpoint(),
    )
    .await
    .map(|_| ())
}

// ── login_device_flow ─────────────────────────────────────────────────────────

/// CLI / operator helper: print codes to stderr and await the full flow.
///
/// Resolves client credentials, starts the device flow, prints the
/// `user_code` and `verification_uri` to stderr, then awaits the background
/// JoinHandle to completion.  Returns `Ok(())` on success.
pub async fn login_device_flow() -> Result<(), OauthError> {
    let creds = resolve_client_creds();
    let (user_code, verification_uri, handle) =
        start_device_code_flow(&creds.client_id, &creds.client_secret).await?;

    eprintln!("\nVisit: {verification_uri}\nEnter code: {user_code}\n");

    let google_creds: GoogleCredentials = handle
        .await
        .map_err(|e| OauthError::Io(std::io::Error::other(e.to_string())))??;

    eprintln!(
        "Google OAuth device login successful. Logged in as: {}",
        google_creds.email
    );
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// `poll_device_flow` succeeds when the token endpoint returns 200.
    #[tokio::test]
    #[serial(oauth_env)]
    async fn poll_device_flow_succeeds_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "final_access",
                    "refresh_token": "final_refresh",
                    "expires_in": 3600
                })),
            )
            .mount(&server)
            .await;

        // Userinfo — best-effort, may return empty email.
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "email": "test@example.com" })),
            )
            .mount(&server)
            .await;

        let session = DeviceFlowSession {
            device_code: "dev_code".to_string(),
            user_code: "ABCD-1234".to_string(),
            verification_uri: "https://google.com/device".to_string(),
            interval_secs: 0,
            expires_in_secs: 60,
        };

        // Override the token endpoint so poll hits the mock server.
        unsafe {
            std::env::set_var(
                "OPEX_GEMINI_TEST_TOKEN_ENDPOINT",
                format!("{}/token", server.uri()),
            );
        }

        let result = poll_device_flow(&session).await;

        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT");
        }

        assert!(result.is_ok(), "poll must succeed on 200: {result:?}");
    }

    /// `poll_device_flow` maps `expired_token` to `OauthError::DeviceExpired`.
    #[tokio::test]
    #[serial(oauth_env)]
    async fn poll_device_flow_expired_token_aborts() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "expired_token" })),
            )
            .mount(&server)
            .await;

        let session = DeviceFlowSession {
            device_code: "dev".to_string(),
            user_code: "ZZ".to_string(),
            verification_uri: "https://google.com/device".to_string(),
            interval_secs: 0,
            expires_in_secs: 3600,
        };

        unsafe {
            std::env::set_var(
                "OPEX_GEMINI_TEST_TOKEN_ENDPOINT",
                format!("{}/token", server.uri()),
            );
        }

        let result = poll_device_flow(&session).await;

        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT");
        }

        assert!(
            matches!(result, Err(OauthError::DeviceExpired)),
            "expired_token must abort: {result:?}"
        );
    }

    /// `poll_device_flow` maps `access_denied` to `OauthError::DeviceAccessDenied`.
    #[tokio::test]
    #[serial(oauth_env)]
    async fn poll_device_flow_access_denied_returns_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "access_denied" })),
            )
            .mount(&server)
            .await;

        let session = DeviceFlowSession {
            device_code: "dev".to_string(),
            user_code: "ZZ".to_string(),
            verification_uri: "https://google.com/device".to_string(),
            interval_secs: 0,
            expires_in_secs: 3600,
        };

        unsafe {
            std::env::set_var(
                "OPEX_GEMINI_TEST_TOKEN_ENDPOINT",
                format!("{}/token", server.uri()),
            );
        }

        let result = poll_device_flow(&session).await;

        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT");
        }

        assert!(
            matches!(result, Err(OauthError::DeviceAccessDenied)),
            "access_denied must map to DeviceAccessDenied: {result:?}"
        );
    }

    /// `start_device_code_flow` (re-exported) resolves and returns user code.
    #[tokio::test]
    #[serial(oauth_env)]
    async fn start_device_code_flow_returns_user_code() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "device_code": "dc123",
                    "user_code": "WXYZ-5678",
                    "verification_url": "https://google.com/device",
                    "interval": 5,
                    "expires_in": 1800
                })),
            )
            .mount(&server)
            .await;

        unsafe {
            std::env::set_var(
                "OPEX_GEMINI_TEST_DEVICE_CODE_ENDPOINT",
                server.uri(),
            );
        }

        let result =
            start_device_code_flow("test-client-id", "test-client-secret").await;

        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_DEVICE_CODE_ENDPOINT");
        }

        let (user_code, verification_uri, handle) =
            result.expect("start_device_code_flow must succeed");
        handle.abort(); // Don't wait for polling in this test.

        assert_eq!(user_code, "WXYZ-5678");
        assert!(verification_uri.contains("google.com/device"));
    }
}
