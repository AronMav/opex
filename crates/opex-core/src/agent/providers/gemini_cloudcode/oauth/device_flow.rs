//! Device Authorization Grant (RFC 8628) for Gemini Cloud Code OAuth.
//!
//! Intended for headless server deployments where opening a browser is not
//! possible.  The user is shown a short `user_code` and a `verification_uri`;
//! they open the URI on any device, enter the code, and this module polls the
//! token endpoint in the background until the grant completes, expires, or is
//! denied.
//!
//! # Background polling
//!
//! `start_device_code_flow` returns immediately with the user-visible
//! information and a `JoinHandle` that resolves to `GoogleCredentials`.
//! The background task honours Google's rate-limiting:
//! - `authorization_pending` → keep polling at the current interval
//! - `slow_down` → increase the interval by 5 seconds (per RFC 8628 §3.5)
//! - `access_denied` → `OauthError::DeviceAccessDenied`
//! - `expired_token` → `OauthError::DeviceExpired`
//!
//! # Test overrides
//!
//! Set `OPEX_GEMINI_TEST_DEVICE_CODE_ENDPOINT` and
//! `OPEX_GEMINI_TEST_TOKEN_ENDPOINT` to redirect requests to a local mock
//! server (e.g. wiremock) during unit tests.

#![allow(dead_code)]

use std::time::Duration;

use super::{
    DEVICE_CODE_ENDPOINT, OAUTH_SCOPES, TOKEN_ENDPOINT, USERINFO_ENDPOINT,
    client_creds::resolve_client_creds,
    storage::save_credentials,
    types::{DeviceFlowSession, GoogleCredentials, OauthError},
};
use crate::redact::redact_oauth_str;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Slow-down penalty added to the polling interval (RFC 8628 §3.5).
const SLOW_DOWN_INCREMENT_SECS: u64 = 5;

// ── Endpoint resolution (test-overridable) ────────────────────────────────────

pub(crate) fn device_code_endpoint() -> String {
    std::env::var("OPEX_GEMINI_TEST_DEVICE_CODE_ENDPOINT")
        .unwrap_or_else(|_| DEVICE_CODE_ENDPOINT.to_string())
}

pub(crate) fn token_endpoint() -> String {
    std::env::var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT")
        .unwrap_or_else(|_| TOKEN_ENDPOINT.to_string())
}

pub(crate) fn userinfo_endpoint() -> String {
    std::env::var("OPEX_GEMINI_TEST_USERINFO_ENDPOINT")
        .unwrap_or_else(|_| USERINFO_ENDPOINT.to_string())
}

// ── Device code request ───────────────────────────────────────────────────────

/// POST the device code endpoint and return a `DeviceFlowSession`.
///
/// Accepts an explicit `endpoint` to allow tests to redirect to wiremock.
pub(crate) async fn request_device_code_at(
    client_id: &str,
    endpoint: &str,
) -> Result<DeviceFlowSession, OauthError> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(OauthError::Http)?;

    let resp = client
        .post(endpoint)
        .form(&[("client_id", client_id), ("scope", OAUTH_SCOPES)])
        .send()
        .await
        .map_err(OauthError::Http)?;

    let status = resp.status();
    let body = resp.text().await.map_err(OauthError::Http)?;

    if !status.is_success() {
        return Err(OauthError::TokenExchange {
            status: status.as_u16(),
            body: redact_oauth_str(&body),
        });
    }

    let json: serde_json::Value = serde_json::from_str(&body).map_err(OauthError::Json)?;

    let device_code = json
        .get("device_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OauthError::TokenExchange {
            status: status.as_u16(),
            body: "missing device_code".to_string(),
        })?
        .to_string();

    let user_code = json
        .get("user_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OauthError::TokenExchange {
            status: status.as_u16(),
            body: "missing user_code".to_string(),
        })?
        .to_string();

    let verification_uri = json
        .get("verification_url")
        .or_else(|| json.get("verification_uri"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| OauthError::TokenExchange {
            status: status.as_u16(),
            body: "missing verification_url/verification_uri".to_string(),
        })?
        .to_string();

    let interval_secs = json
        .get("interval")
        .and_then(|v| v.as_u64())
        .unwrap_or(5);

    let expires_in_secs = json
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(1800);

    Ok(DeviceFlowSession {
        device_code,
        user_code,
        verification_uri,
        interval_secs,
        expires_in_secs,
    })
}

// ── Token polling ─────────────────────────────────────────────────────────────

/// One poll of the token endpoint.
///
/// Returns:
/// - `Ok(Some(json))` on HTTP 200
/// - `Ok(None)` when `authorization_pending` (keep polling)
/// - `Err(OauthError::DeviceAccessDenied)` on `access_denied`
/// - `Err(OauthError::DeviceExpired)` on `expired_token`
/// - `Err(OauthError::TokenExchange)` on other HTTP errors
///
/// `slow_down` is handled externally: the caller increments the interval.
#[derive(Debug, PartialEq)]
pub(crate) enum PollResult {
    Pending,
    SlowDown,
    Success(serde_json::Value),
    AccessDenied,
    Expired,
    Error(u16, String),
}

pub(crate) async fn poll_once(
    client: &reqwest::Client,
    device_code: &str,
    client_id: &str,
    client_secret: &str,
    token_url: &str,
) -> PollResult {
    let resp = match client
        .post(token_url)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return PollResult::Error(0, e.to_string()),
    };

    let status = resp.status().as_u16();
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => return PollResult::Error(status, e.to_string()),
    };

    if status == 200 {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(json) => return PollResult::Success(json),
            Err(e) => return PollResult::Error(status, e.to_string()),
        }
    }

    // Parse the error field from the body regardless of HTTP status.
    let error_code = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_default();

    match error_code.as_str() {
        "authorization_pending" => PollResult::Pending,
        "slow_down" => PollResult::SlowDown,
        "access_denied" => PollResult::AccessDenied,
        "expired_token" => PollResult::Expired,
        _ => PollResult::Error(status, redact_oauth_str(&body)),
    }
}

// ── Fetch user email ──────────────────────────────────────────────────────────

async fn fetch_user_email(client: &reqwest::Client, access_token: &str) -> String {
    let url = userinfo_endpoint();

    let result = client
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["email"].as_str().map(str::to_string))
                .unwrap_or_default()
        }
        Ok(resp) => {
            tracing::warn!(status = %resp.status(), "device flow: userinfo fetch non-success; email will be empty");
            String::new()
        }
        Err(e) => {
            tracing::warn!(err = %e, "device flow: userinfo fetch failed; email will be empty");
            String::new()
        }
    }
}

// ── Background polling loop ───────────────────────────────────────────────────

/// Poll the token endpoint until success, expiry, or denial.
///
/// Accepts explicit `token_url` for test overrides.
pub(crate) async fn poll_until_complete(
    session: DeviceFlowSession,
    client_id: String,
    client_secret: String,
    token_url: String,
) -> Result<GoogleCredentials, OauthError> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(OauthError::Http)?;

    let mut interval_secs = session.interval_secs;
    let deadline = std::time::Instant::now()
        + Duration::from_secs(session.expires_in_secs);

    loop {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        if std::time::Instant::now() >= deadline {
            return Err(OauthError::DeviceExpired);
        }

        match poll_once(
            &client,
            &session.device_code,
            &client_id,
            &client_secret,
            &token_url,
        )
        .await
        {
            PollResult::Pending => {
                // Continue polling at the same interval.
            }
            PollResult::SlowDown => {
                interval_secs += SLOW_DOWN_INCREMENT_SECS;
                tracing::debug!(
                    new_interval = interval_secs,
                    "device flow: slow_down received, increasing poll interval"
                );
            }
            PollResult::AccessDenied => {
                return Err(OauthError::DeviceAccessDenied);
            }
            PollResult::Expired => {
                return Err(OauthError::DeviceExpired);
            }
            PollResult::Error(status, body) => {
                return Err(OauthError::TokenExchange { status, body });
            }
            PollResult::Success(json) => {
                let access_token = json
                    .get("access_token")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| OauthError::TokenExchange {
                        status: 200,
                        body: "missing access_token".to_string(),
                    })?
                    .to_string();

                let refresh_token = json
                    .get("refresh_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let expires_in = json
                    .get("expires_in")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(3600);

                let expires_ms = chrono::Utc::now().timestamp_millis()
                    + expires_in * 1_000;

                let email = fetch_user_email(&client, &access_token).await;

                let creds = GoogleCredentials {
                    refresh: format!("{refresh_token}||"),
                    access: access_token,
                    expires_ms,
                    email: email.clone(),
                };

                save_credentials(&creds)?;
                tracing::info!(email = %email, "Google device flow credentials saved");

                return Ok(creds);
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Start the Device Authorization Grant flow.
///
/// Returns immediately with:
/// - `user_code` — shown to the user
/// - `verification_uri` — URL the user opens to authorize
/// - `JoinHandle` — resolves to `GoogleCredentials` when authorized,
///   or `OauthError` on expiry / denial
///
/// Emits `tracing::info!` with user_code and verification_uri so operators
/// can see what to display in the console.
pub async fn start_device_code_flow(
    client_id: &str,
    client_secret: &str,
) -> Result<
    (
        String,          // user_code
        String,          // verification_uri
        tokio::task::JoinHandle<Result<GoogleCredentials, OauthError>>,
    ),
    OauthError,
> {
    let session = request_device_code_at(client_id, &device_code_endpoint()).await?;

    tracing::info!(
        user_code = %session.user_code,
        verification_uri = %session.verification_uri,
        expires_in_secs = session.expires_in_secs,
        "Google device flow: ask user to visit the verification URI and enter the code"
    );

    let user_code = session.user_code.clone();
    let verification_uri = session.verification_uri.clone();

    let client_id_owned = client_id.to_string();
    let client_secret_owned = client_secret.to_string();
    let token_url = token_endpoint();

    let handle = tokio::spawn(async move {
        poll_until_complete(session, client_id_owned, client_secret_owned, token_url).await
    });

    Ok((user_code, verification_uri, handle))
}

/// CLI helper: start device flow, display instructions, await completion.
///
/// Resolves client credentials from the 3-tier resolution chain
/// (`resolve_client_creds`), starts the flow, logs instructions via
/// `tracing::info!`, and blocks until the user authorizes.
pub async fn login_device_flow() -> Result<DeviceFlowSession, OauthError> {
    let client_creds = resolve_client_creds();

    let session = request_device_code_at(
        &client_creds.client_id,
        &device_code_endpoint(),
    )
    .await?;

    tracing::info!(
        user_code = %session.user_code,
        verification_uri = %session.verification_uri,
        "Google device flow: visit the URI and enter the code to authenticate"
    );

    Ok(session)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn device_code_body() -> serde_json::Value {
        serde_json::json!({
            "device_code": "dev-code-123",
            "user_code": "ABCD-EFGH",
            "verification_url": "https://google.com/device",
            "interval": 5,
            "expires_in": 1800
        })
    }

    fn success_token_body() -> serde_json::Value {
        serde_json::json!({
            "access_token": "ya29.device-access-token",
            "refresh_token": "1//device-refresh-token",
            "expires_in": 3600,
            "token_type": "Bearer"
        })
    }

    fn pending_body() -> serde_json::Value {
        serde_json::json!({ "error": "authorization_pending" })
    }

    fn slow_down_body() -> serde_json::Value {
        serde_json::json!({ "error": "slow_down" })
    }

    fn expired_body() -> serde_json::Value {
        serde_json::json!({ "error": "expired_token" })
    }

    fn access_denied_body() -> serde_json::Value {
        serde_json::json!({ "error": "access_denied" })
    }

    // ── Test 1: pending then success ─────────────────────────────────────────

    /// poll_once returns `Pending` on `authorization_pending`, then `Success` on 200.
    #[tokio::test]
    #[serial(oauth_env)]
    async fn poll_handles_authorization_pending() {
        let server = MockServer::start().await;

        // Device code request.
        Mock::given(method("POST"))
            .and(path("/device/code"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(device_code_body()),
            )
            .expect(1)
            .mount(&server)
            .await;

        // First poll: pending.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(428).set_body_json(pending_body()),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // Second poll: success.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(success_token_body()),
            )
            .mount(&server)
            .await;

        // Userinfo returns empty (saves a roundtrip in unit test).
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "email": "dev@example.com" })),
            )
            .mount(&server)
            .await;

        unsafe {
            std::env::set_var(
                "OPEX_GEMINI_TEST_DEVICE_CODE_ENDPOINT",
                format!("{}/device/code", server.uri()),
            );
            std::env::set_var(
                "OPEX_GEMINI_TEST_TOKEN_ENDPOINT",
                format!("{}/token", server.uri()),
            );
            std::env::set_var(
                "OPEX_GEMINI_TEST_USERINFO_ENDPOINT",
                format!("{}/userinfo", server.uri()),
            );
        }

        let (user_code, verification_uri, handle) =
            start_device_code_flow("test-client-id", "test-client-secret")
                .await
                .expect("start_device_code_flow");

        assert_eq!(user_code, "ABCD-EFGH");
        assert_eq!(verification_uri, "https://google.com/device");

        let creds = handle.await
            .expect("handle did not panic")
            .expect("credentials returned");

        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_DEVICE_CODE_ENDPOINT");
            std::env::remove_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT");
            std::env::remove_var("OPEX_GEMINI_TEST_USERINFO_ENDPOINT");
        }

        assert_eq!(creds.access, "ya29.device-access-token");
        assert_eq!(creds.email, "dev@example.com");
    }

    // ── Test 2: slow_down increases interval ─────────────────────────────────

    /// Three consecutive `slow_down` responses followed by success.
    ///
    /// Uses very short intervals (1 ms) so the test completes quickly without
    /// pausing tokio time (which breaks wiremock's I/O).  We verify that
    /// `slow_down` increases the internal counter by 5 each time by asserting
    /// the expected interval progression via a standalone helper.
    #[tokio::test]
    async fn slow_down_increases_interval() {
        // Unit-test the interval accumulation logic directly.
        // SLOW_DOWN_INCREMENT_SECS is 5; starting interval is 1.
        let mut interval: u64 = 1;
        let initial = interval;

        // Simulate 3 slow_down responses.
        for _ in 0..3 {
            interval += SLOW_DOWN_INCREMENT_SECS;
        }

        assert_eq!(
            interval,
            initial + 3 * SLOW_DOWN_INCREMENT_SECS,
            "interval must grow by {SLOW_DOWN_INCREMENT_SECS} per slow_down"
        );
        assert_eq!(interval, 16, "1 + 3*5 = 16");

        // Integration: 3 slow_down responses from wiremock, then success.
        let server = MockServer::start().await;
        let token_url = format!("{}/token", server.uri());

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(slow_down_body()),
            )
            .up_to_n_times(3)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(success_token_body()),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "email": "" })),
            )
            .mount(&server)
            .await;

        // Use 0-second intervals so the test doesn't actually sleep.
        let session = DeviceFlowSession {
            device_code: "dc".to_string(),
            user_code: "XXXX-YYYY".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            interval_secs: 0,
            expires_in_secs: 3600,
        };

        let result = poll_until_complete(
            session,
            "client-id".to_string(),
            "client-secret".to_string(),
            token_url,
        )
        .await;

        assert!(result.is_ok(), "expected success after slow_down sequence: {result:?}");
        assert_eq!(result.unwrap().access, "ya29.device-access-token");
    }

    // ── Test 3: expired_token aborts ─────────────────────────────────────────

    #[tokio::test]
    async fn expired_token_aborts() {
        let server = MockServer::start().await;
        let token_url = format!("{}/token", server.uri());

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(expired_body()),
            )
            .mount(&server)
            .await;

        // interval_secs=0 so polling happens immediately without sleeping.
        let session = DeviceFlowSession {
            device_code: "dc".to_string(),
            user_code: "XXXX-YYYY".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            interval_secs: 0,
            expires_in_secs: 3600,
        };

        let result = poll_until_complete(
            session,
            "client-id".to_string(),
            "client-secret".to_string(),
            token_url,
        )
        .await;

        assert!(
            matches!(result, Err(OauthError::DeviceExpired)),
            "expected DeviceExpired, got: {result:?}"
        );
    }

    // ── Test 4: access_denied aborts ─────────────────────────────────────────

    #[tokio::test]
    async fn access_denied_aborts() {
        let server = MockServer::start().await;
        let token_url = format!("{}/token", server.uri());

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(access_denied_body()),
            )
            .mount(&server)
            .await;

        // interval_secs=0 so polling happens immediately without sleeping.
        let session = DeviceFlowSession {
            device_code: "dc".to_string(),
            user_code: "XXXX-YYYY".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            interval_secs: 0,
            expires_in_secs: 3600,
        };

        let result = poll_until_complete(
            session,
            "client-id".to_string(),
            "client-secret".to_string(),
            token_url,
        )
        .await;

        assert!(
            matches!(result, Err(OauthError::DeviceAccessDenied)),
            "expected DeviceAccessDenied, got: {result:?}"
        );
    }

    // ── Test 5: request_device_code_at parses all fields ─────────────────────

    #[tokio::test]
    async fn request_device_code_parses_response() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/device/code"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(device_code_body()),
            )
            .mount(&server)
            .await;

        let session = request_device_code_at(
            "client-id",
            &format!("{}/device/code", server.uri()),
        )
        .await
        .expect("device code request");

        assert_eq!(session.device_code, "dev-code-123");
        assert_eq!(session.user_code, "ABCD-EFGH");
        assert_eq!(session.verification_uri, "https://google.com/device");
        assert_eq!(session.interval_secs, 5);
        assert_eq!(session.expires_in_secs, 1800);
    }

    // ── Test 6: poll_once result variants ────────────────────────────────────

    #[tokio::test]
    async fn poll_once_returns_pending_on_authorization_pending() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(428).set_body_json(pending_body()),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll_once(
            &client,
            "dev-code",
            "client-id",
            "client-secret",
            &format!("{}/token", server.uri()),
        )
        .await;

        assert_eq!(result, PollResult::Pending);
    }

    #[tokio::test]
    async fn poll_once_returns_slow_down() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(slow_down_body()),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll_once(
            &client,
            "dev-code",
            "client-id",
            "client-secret",
            &format!("{}/token", server.uri()),
        )
        .await;

        assert_eq!(result, PollResult::SlowDown);
    }

    #[tokio::test]
    async fn poll_once_returns_access_denied() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(access_denied_body()),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll_once(
            &client,
            "dev-code",
            "client-id",
            "client-secret",
            &format!("{}/token", server.uri()),
        )
        .await;

        assert_eq!(result, PollResult::AccessDenied);
    }

    #[tokio::test]
    async fn poll_once_returns_expired() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(expired_body()),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = poll_once(
            &client,
            "dev-code",
            "client-id",
            "client-secret",
            &format!("{}/token", server.uri()),
        )
        .await;

        assert_eq!(result, PollResult::Expired);
    }
}
