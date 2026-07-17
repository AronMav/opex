//! OAuth 2.0 Authorization Code + PKCE flow with a local HTTP callback server.
//!
//! # Overview
//!
//! `start_authorization_code_flow` binds a loopback TCP listener on port 8085
//! (or the next free port in 8085–8089), generates a PKCE pair and state token,
//! builds the Google authorization URL, and spawns a background task that waits
//! for the callback.  The caller receives the authorization URL, the state token,
//! and a `JoinHandle` that resolves to `GoogleCredentials` on success or an
//! `OauthError` on failure/timeout.
//!
//! # Policy warning
//!
//! Per Google's ToS, using their published OAuth client with third-party software
//! is a policy violation.  This module emits a `tracing::warn!` on every call.
//!
//! # Callback server
//!
//! A minimal raw TCP handler parses one HTTP/1.x GET request on the loopback
//! interface, extracts `code` and `state` query params, validates state, exchanges
//! the code for tokens, fetches the user email (best-effort), saves credentials,
//! then sends a plain-text HTTP 200 response and shuts down.
//!
//! # Test overrides
//!
//! Set `OPEX_GEMINI_TEST_TOKEN_ENDPOINT` and
//! `OPEX_GEMINI_TEST_USERINFO_ENDPOINT` to redirect token exchange and
//! userinfo to a local mock server (e.g. wiremock) during unit tests.
//! These env vars are only consulted at call time — not at module init — so
//! each test can set them independently.

// Suppress dead-code lints on items used only in later tasks.
#![allow(dead_code)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{
    AUTH_ENDPOINT, CALLBACK_PATH, DEFAULT_REDIRECT_PORT, OAUTH_SCOPES, REDIRECT_HOST,
    TOKEN_ENDPOINT, USERINFO_ENDPOINT,
};
use super::pkce::{generate_pkce_pair, generate_state};
use super::storage::save_credentials;
use super::types::{GoogleCredentials, OauthError};
use crate::redact::{redact_oauth_str, redact_token_in_url};

// ── Constants ─────────────────────────────────────────────────────────────────

const PORT_RANGE_END: u16 = 8089;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
const HTTP_RESPONSE_OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nAuthentication successful. You may close this window.";
const HTTP_RESPONSE_ERR: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nAuthentication failed. You may close this window.";

// ── Endpoint resolution (test-overridable) ─────────────────────────────────────

fn token_endpoint() -> String {
    std::env::var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT")
        .unwrap_or_else(|_| TOKEN_ENDPOINT.to_string())
}

fn userinfo_endpoint() -> String {
    std::env::var("OPEX_GEMINI_TEST_USERINFO_ENDPOINT")
        .unwrap_or_else(|_| USERINFO_ENDPOINT.to_string())
}

// ── Port binding ──────────────────────────────────────────────────────────────

/// Try to bind `127.0.0.1:{port}`. Returns `Ok(listener)` on success.
async fn try_bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(format!("{REDIRECT_HOST}:{port}")).await
}

/// Find the first free port in the hint..=PORT_RANGE_END range.
/// Returns `(TcpListener, bound_port)` or `OauthError::PortBindFailed`.
async fn bind_callback_port(hint: Option<u16>) -> Result<(TcpListener, u16), OauthError> {
    let start = hint.unwrap_or(DEFAULT_REDIRECT_PORT);
    let end = PORT_RANGE_END.max(start);

    for port in start..=end {
        match try_bind(port).await {
            Ok(listener) => return Ok((listener, port)),
            Err(_) => continue,
        }
    }
    Err(OauthError::PortBindFailed)
}

// ── Authorization URL builder ─────────────────────────────────────────────────

fn build_auth_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    pkce_challenge: &str,
) -> String {
    let mut url = url::Url::parse(AUTH_ENDPOINT).expect("AUTH_ENDPOINT is valid");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", client_id);
        q.append_pair("redirect_uri", redirect_uri);
        q.append_pair("scope", OAUTH_SCOPES);
        q.append_pair("state", state);
        q.append_pair("code_challenge", pkce_challenge);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("access_type", "offline");
        q.append_pair("prompt", "consent");
    }
    url.to_string()
}

// ── Minimal HTTP request parser ───────────────────────────────────────────────

/// Parsed query params from the callback GET request.
#[derive(Debug)]
struct CallbackParams {
    code: String,
    state: String,
}

/// Read the first HTTP request line from a raw TCP stream and extract query params.
///
/// Reads up to 8 KiB — enough for any OAuth callback URL.
async fn parse_callback_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<CallbackParams, OauthError> {
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // First line: "GET /oauth2callback?code=...&state=... HTTP/1.1"
    let first_line = request.lines().next().unwrap_or("");
    let path_with_query = first_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("");

    // Parse query string from the path.
    let query_str = path_with_query
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or("");

    let mut code = None;
    let mut state = None;

    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(urlencoding::decode(v).unwrap_or_default().into_owned()),
                "state" => state = Some(urlencoding::decode(v).unwrap_or_default().into_owned()),
                _ => {}
            }
        }
    }

    match (code, state) {
        (Some(code), Some(state)) if !code.is_empty() && !state.is_empty() => {
            Ok(CallbackParams { code, state })
        }
        _ => Err(OauthError::TokenExchange {
            status: 400,
            body: "callback missing code or state parameter".to_string(),
        }),
    }
}

// ── Token exchange ────────────────────────────────────────────────────────────

/// Exchange an authorization code for tokens.
async fn exchange_code(
    client: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    code: &str,
    pkce_verifier: &str,
    redirect_uri: &str,
) -> Result<(String, String, i64), OauthError> {
    let resp = client
        .post(token_endpoint())
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pkce_verifier),
        ])
        .send()
        .await?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();

    if status != 200 {
        let safe_body = redact_oauth_str(&body);
        return Err(OauthError::TokenExchange {
            status,
            body: safe_body,
        });
    }

    let json: serde_json::Value = serde_json::from_str(&body)?;

    let access_token = json["access_token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let refresh_token = json["refresh_token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let expires_in = json["expires_in"].as_i64().unwrap_or(3600);

    let expires_ms = chrono::Utc::now().timestamp_millis()
        + expires_in * 1_000;

    Ok((access_token, refresh_token, expires_ms))
}

// ── Userinfo fetch ────────────────────────────────────────────────────────────

/// Fetch user email from the userinfo endpoint. Best-effort — returns empty string on failure.
async fn fetch_user_email(client: &reqwest::Client, access_token: &str) -> String {
    let url = userinfo_endpoint();
    tracing::debug!(url = %redact_token_in_url(&url), "fetching userinfo");

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
            tracing::warn!(status = %resp.status(), "userinfo fetch non-success; email will be empty");
            String::new()
        }
        Err(e) => {
            tracing::warn!(err = %e, "userinfo fetch failed; email will be empty");
            String::new()
        }
    }
}

// ── Callback server task ──────────────────────────────────────────────────────

/// Background task: wait for the OAuth callback, validate, exchange, save.
async fn run_callback_server(
    listener: TcpListener,
    client_id: String,
    client_secret: String,
    expected_state: String,
    pkce_verifier: String,
    redirect_uri: String,
    cancel: CancellationToken,
) -> Result<GoogleCredentials, OauthError> {
    let http_client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(OauthError::Http)?;

    tokio::select! {
        _ = cancel.cancelled() => {
            Err(OauthError::Timeout)
        }
        result = wait_for_callback(
            listener,
            &http_client,
            &client_id,
            &client_secret,
            &expected_state,
            &pkce_verifier,
            &redirect_uri,
        ) => {
            result
        }
    }
}

async fn wait_for_callback(
    listener: TcpListener,
    http_client: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    expected_state: &str,
    pkce_verifier: &str,
    redirect_uri: &str,
) -> Result<GoogleCredentials, OauthError> {
    // Accept one connection (the browser redirect).
    let (mut stream, _peer) = listener.accept().await?;

    let params = parse_callback_request(&mut stream).await;

    // Validate state before doing anything with code.
    let params = match params {
        Ok(p) if p.state != expected_state => {
            let _ = stream.write_all(HTTP_RESPONSE_ERR).await;
            return Err(OauthError::StateMismatch);
        }
        Ok(p) => p,
        Err(e) => {
            let _ = stream.write_all(HTTP_RESPONSE_ERR).await;
            return Err(e);
        }
    };

    // Exchange code for tokens.
    let (access_token, refresh_token, expires_ms) = match exchange_code(
        http_client,
        client_id,
        client_secret,
        &params.code,
        pkce_verifier,
        redirect_uri,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            let _ = stream.write_all(HTTP_RESPONSE_ERR).await;
            return Err(e);
        }
    };

    // Fetch email (best-effort).
    let email = fetch_user_email(http_client, &access_token).await;

    // Send success response to the browser.
    let _ = stream.write_all(HTTP_RESPONSE_OK).await;
    drop(stream);

    // Build and persist credentials.
    let creds = GoogleCredentials {
        refresh: format!("{refresh_token}||"),
        access: access_token,
        expires_ms,
        email,
    };

    save_credentials(&creds)?;
    tracing::info!(email = %creds.email, "Google OAuth credentials saved");

    Ok(creds)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Start the Authorization Code + PKCE flow.
///
/// Returns:
/// - `auth_url` — the URL the user must open in their browser
/// - `state` — the CSRF state token (for logging/display)
/// - `JoinHandle` — resolves to `GoogleCredentials` when the callback arrives,
///   or `OauthError` on state-mismatch, timeout, or exchange failure
///
/// The background task automatically times out after 5 minutes.
///
/// # Policy warning
/// Using Google's published OAuth client with third-party software is a
/// Google policy violation (ToS §3).  This function emits a `tracing::warn!`
/// on every call so operators can see it on the console.
pub async fn start_authorization_code_flow(
    client_id: &str,
    client_secret: &str,
    redirect_port_hint: Option<u16>,
) -> Result<
    (
        String,
        String,
        JoinHandle<Result<GoogleCredentials, OauthError>>,
    ),
    OauthError,
> {
    tracing::warn!(
        "Using Google's OAuth client with third-party software is a Google policy violation."
    );

    let (listener, port) = bind_callback_port(redirect_port_hint).await?;
    let redirect_uri = format!("http://{REDIRECT_HOST}:{port}{CALLBACK_PATH}");

    let pkce = generate_pkce_pair();
    let state = generate_state();

    let auth_url = build_auth_url(client_id, &redirect_uri, &state, &pkce.challenge);

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let client_id = client_id.to_string();
    let client_secret = client_secret.to_string();
    let state_clone = state.clone();

    let handle = tokio::spawn(async move {
        let result = tokio::time::timeout(
            CALLBACK_TIMEOUT,
            run_callback_server(
                listener,
                client_id,
                client_secret,
                state_clone,
                pkce.verifier,
                redirect_uri,
                cancel_clone,
            ),
        )
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => Err(OauthError::Timeout),
        }
    });

    // Drop the cancellation token ownership here — the task holds the clone.
    // Callers that want early cancellation can wrap the handle in their own cancel logic.
    drop(cancel);

    Ok((auth_url, state, handle))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tokio::net::TcpListener as RawListener;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── Helper: drive an HTTP GET to the callback URL ─────────────────────────

    /// Send a simulated browser redirect to the local callback server.
    async fn send_callback(port: u16, code: &str, state: &str) {
        use tokio::io::AsyncWriteExt;
        let addr = format!("{REDIRECT_HOST}:{port}");
        let mut conn = tokio::net::TcpStream::connect(&addr).await
            .expect("connect to callback server");
        let request = format!(
            "GET {CALLBACK_PATH}?code={code}&state={state} HTTP/1.1\r\nHost: {REDIRECT_HOST}:{port}\r\n\r\n"
        );
        conn.write_all(request.as_bytes()).await.expect("send callback request");
        // Read and discard the response.
        let mut buf = vec![0u8; 512];
        let _ = conn.read(&mut buf).await;
    }

    // ── Test 1: full PKCE flow with mock endpoints ─────────────────────────────

    #[tokio::test]
    #[serial(oauth_env)]
    async fn full_pkce_flow_with_mock_callback() {
        // Start wiremock for token endpoint.
        let token_server = MockServer::start().await;
        let userinfo_server = MockServer::start().await;

        // Token exchange returns access + refresh token.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "test-access-token",
                    "refresh_token": "test-refresh-token",
                    "expires_in": 3600,
                    "token_type": "Bearer"
                })),
            )
            .mount(&token_server)
            .await;

        // Userinfo returns email.
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "email": "test@example.com"
                })),
            )
            .mount(&userinfo_server)
            .await;

        // Set test env vars to redirect endpoints to mocks.
        let token_url = format!("{}/token", token_server.uri());
        let userinfo_url = format!("{}/userinfo", userinfo_server.uri());

        unsafe {
            std::env::set_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT", &token_url);
            std::env::set_var("OPEX_GEMINI_TEST_USERINFO_ENDPOINT", &userinfo_url);
        }

        // Start the flow.
        let (auth_url, state, handle) =
            start_authorization_code_flow("test-client-id", "test-client-secret", None)
                .await
                .expect("start_authorization_code_flow");

        // Extract bound port from the redirect_uri in the auth_url.
        let parsed = url::Url::parse(&auth_url).expect("valid auth_url");
        let redirect_uri_param = parsed
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .expect("redirect_uri param");
        let callback_port = url::Url::parse(&redirect_uri_param)
            .expect("valid redirect_uri")
            .port()
            .expect("port in redirect_uri");

        // Simulate browser sending the callback.
        send_callback(callback_port, "test-auth-code", &state).await;

        // Await the result.
        let creds = handle.await
            .expect("handle did not panic")
            .expect("credentials returned");

        // Cleanup env vars.
        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT");
            std::env::remove_var("OPEX_GEMINI_TEST_USERINFO_ENDPOINT");
        }

        assert_eq!(creds.email, "test@example.com");
        assert_eq!(creds.access, "test-access-token");
        assert!(creds.refresh.starts_with("test-refresh-token"));
        assert!(creds.expires_ms > 0);
    }

    // ── Test 2: state mismatch rejects the callback ────────────────────────────

    #[tokio::test]
    #[serial(oauth_env)]
    async fn state_mismatch_rejects() {
        let token_server = MockServer::start().await;

        // Token endpoint should NOT be called; but we set it up defensively.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "should-not-reach",
                "refresh_token": "x",
                "expires_in": 3600
            })))
            .mount(&token_server)
            .await;

        let token_url = format!("{}/token", token_server.uri());

        unsafe {
            std::env::set_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT", &token_url);
            std::env::remove_var("OPEX_GEMINI_TEST_USERINFO_ENDPOINT");
        }

        let (_auth_url, _real_state, handle) =
            start_authorization_code_flow("id", "secret", None)
                .await
                .expect("start flow");

        // Extract port from auth_url.
        let parsed = url::Url::parse(&_auth_url).expect("valid auth_url");
        let redirect_uri_param = parsed
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .expect("redirect_uri param");
        let port = url::Url::parse(&redirect_uri_param)
            .expect("valid redirect_uri")
            .port()
            .expect("port");

        // Send adversarial callback with WRONG state.
        send_callback(port, "some-code", "WRONG-STATE-TOTALLY-INVALID").await;

        let result = handle.await.expect("handle did not panic");

        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT");
        }

        assert!(
            matches!(result, Err(OauthError::StateMismatch)),
            "expected StateMismatch, got: {result:?}"
        );
    }

    // ── Test 3: port fallback when primary port is busy ─────────────────────────

    #[tokio::test]
    async fn port_bind_falls_back_to_next() {
        // Try to bind the default port; skip the test if it's already in use
        // (happens under parallel test execution when another test holds it).
        let occupier = match RawListener::bind(format!("{REDIRECT_HOST}:{DEFAULT_REDIRECT_PORT}")).await {
            Ok(l) => l,
            Err(_) => {
                // Port 8085 already busy — can't run this test meaningfully.
                // Skip rather than panic.
                return;
            }
        };

        // Now the flow must use the next port (8086+) since we hold 8085.
        let result = start_authorization_code_flow("test-id", "test-secret", None).await;

        // The occupier holds 8085; the flow should bind 8086 successfully.
        match result {
            Ok((auth_url, _state, handle)) => {
                // Confirm port is NOT the default.
                let parsed = url::Url::parse(&auth_url).expect("valid auth_url");
                let redirect_uri_param = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "redirect_uri")
                    .map(|(_, v)| v.into_owned())
                    .expect("redirect_uri");
                let port = url::Url::parse(&redirect_uri_param)
                    .expect("redirect_uri url")
                    .port()
                    .expect("port");

                assert_ne!(port, DEFAULT_REDIRECT_PORT, "must NOT use occupied port");
                assert!(
                    port > DEFAULT_REDIRECT_PORT && port <= PORT_RANGE_END,
                    "port {port} not in fallback range"
                );

                // Clean up: abort the handle so the listener is dropped.
                handle.abort();
            }
            Err(OauthError::PortBindFailed) => {
                // All ports in range busy — acceptable on heavily loaded CI.
            }
            Err(e) => panic!("unexpected error: {e}"),
        }

        // Keep the occupier alive until here so it's not dropped early.
        drop(occupier);
    }

    // ── Test 4: auth URL contains required OAuth parameters ────────────────────

    #[test]
    fn auth_url_contains_required_params() {
        let url = build_auth_url(
            "my-client-id",
            "http://127.0.0.1:8085/oauth2callback",
            "my-state",
            "my-challenge",
        );
        let parsed = url::Url::parse(&url).expect("valid url");
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().collect();

        assert_eq!(params.get("response_type").map(|s| s.as_ref()), Some("code"));
        assert_eq!(params.get("client_id").map(|s| s.as_ref()), Some("my-client-id"));
        assert_eq!(params.get("state").map(|s| s.as_ref()), Some("my-state"));
        assert_eq!(params.get("code_challenge").map(|s| s.as_ref()), Some("my-challenge"));
        assert_eq!(params.get("code_challenge_method").map(|s| s.as_ref()), Some("S256"));
        assert_eq!(params.get("access_type").map(|s| s.as_ref()), Some("offline"));
        assert!(params.contains_key("scope"), "scope must be present");
    }

    // ── Test 5: all ports busy returns PortBindFailed ──────────────────────────

    #[tokio::test]
    async fn all_ports_busy_returns_port_bind_failed() {
        // Occupy all ports in the range 8085–8089.
        let mut listeners = Vec::new();
        for port in DEFAULT_REDIRECT_PORT..=PORT_RANGE_END {
            if let Ok(l) = RawListener::bind(format!("{REDIRECT_HOST}:{port}")).await {
                listeners.push(l);
            }
        }

        // Only test if we actually occupied all 5 ports.
        if listeners.len() == (PORT_RANGE_END - DEFAULT_REDIRECT_PORT + 1) as usize {
            let result =
                start_authorization_code_flow("id", "secret", None).await;
            assert!(
                matches!(result, Err(OauthError::PortBindFailed)),
                "expected PortBindFailed when all ports occupied, got {result:?}"
            );
        }
        // If we couldn't occupy all ports (OS restricts binding), skip silently.
        drop(listeners);
    }

    // ── Test 6: parse_callback_request extracts code and state ────────────────

    #[tokio::test]
    async fn parse_callback_request_extracts_params() {
        // Spin up a listener, send a real HTTP request, parse it.
        let listener = RawListener::bind(format!("{REDIRECT_HOST}:0"))
            .await
            .expect("bind random port");
        let port = listener.local_addr().expect("local addr").port();

        let send_task = tokio::spawn(async move {
            let mut conn = tokio::net::TcpStream::connect(format!("{REDIRECT_HOST}:{port}"))
                .await
                .expect("connect");
            let req = "GET /oauth2callback?code=my-code&state=my-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
            conn.write_all(req.as_bytes()).await.expect("write");
        });

        let (mut stream, _) = listener.accept().await.expect("accept");
        let params = parse_callback_request(&mut stream)
            .await
            .expect("parse params");

        send_task.await.expect("sender did not panic");

        assert_eq!(params.code, "my-code");
        assert_eq!(params.state, "my-state");
    }
}
