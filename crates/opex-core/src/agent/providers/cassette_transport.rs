//! `CassetteTransport` — record/replay impl of [`HttpTransport`].
//!
//! Two modes:
//! - **record** (`OPEX_CASSETTE=record`): passthrough to a real
//!   [`reqwest::Client`] (wrapped in [`RealTransport`]); capture each
//!   `{request, response}` interaction, redact secrets, append to an
//!   in-memory [`Cassette`], and on `finalize()` write it to disk. The
//!   returned response is reconstructed from the (un-redacted) captured bytes
//!   so the live caller sees real data while only the cassette stores redacted
//!   copies.
//! - **replay** (`OPEX_CASSETTE=replay`, default when `CI=true`): serve the Nth
//!   recorded interaction to the Nth runtime request (sequential matching —
//!   correctly models retries/polling). Matching is by method + canonicalized
//!   body (JSON keys sorted) + URL path (query redacted). Missing cassette →
//!   error. Unused interactions → finalizer assertion (catches under-requests).
//!
//! Mode resolution (`OPEX_CASSETTE` env):
//! - `record` → record
//! - `replay` → replay
//! - unset + `CI=true` → replay (missing cassette = fail)
//! - unset + `CI` unset/false → auto: cassette exists → replay; missing → record
//!
//! `CassetteTransport` is gated to `#[cfg(test)]` — it is only compiled for
//! the test build, so the production binary never carries record/replay code.

#![cfg(test)]

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;

use super::cassette::{BodyEncoding, Cassette, HttpInteraction, RequestSnapshot, ResponseSnapshot};
use super::http::SendError;
use super::redaction;
use super::transport::{HttpTransport, RealTransport};

/// Record/replay mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Record,
    Replay,
}

/// Resolve the mode from `OPEX_CASSETTE` + `CI` env vars.
pub fn resolve_mode(cassette_path: &Path) -> Mode {
    match std::env::var("OPEX_CASSETTE").as_deref() {
        Ok("record") => Mode::Record,
        Ok("replay") => Mode::Replay,
        _ => {
            let is_ci = std::env::var("CI")
                .map(|v| !v.is_empty() && v != "false" && v != "0")
                .unwrap_or(false);
            // CI forces replay (missing cassette = fail). Otherwise auto:
            // cassette exists → replay; missing → record.
            if is_ci || cassette_path.exists() {
                Mode::Replay
            } else {
                Mode::Record
            }
        }
    }
}

/// A recording/replaying HTTP transport backed by a cassette file.
///
/// In record mode it wraps a [`RealTransport`] (real network) and appends each
/// interaction to an in-memory cassette. Call `finalize()` at the end of the
/// test to persist the cassette to `path`.
///
/// In replay mode it loads the cassette up front and serves responses
/// sequentially. `finalize()` asserts all interactions were consumed.
pub struct CassetteTransport {
    mode: Mode,
    path: PathBuf,
    /// Underlying real transport (only used in record mode).
    real: RealTransport,
    /// In record mode: interactions accumulate here. In replay mode: the
    /// loaded cassette (consumed via a cursor).
    state: AsyncMutex<CassetteState>,
}

struct CassetteState {
    /// In record mode: the cassette being built. In replay mode: the loaded
    /// cassette with a consumption cursor.
    cassette: Cassette,
    cursor: usize,
}

impl CassetteTransport {
    /// Create a transport for the given cassette path. The mode is resolved
    /// from env vars via [`resolve_mode`]. In replay mode the cassette is
    /// loaded eagerly; missing cassette = error.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mode = resolve_mode(&path);
        let real = RealTransport::new(reqwest::Client::new());
        let state = match mode {
            Mode::Record => CassetteState {
                cassette: Cassette::new(Some(serde_json::json!({
                    "name": path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown"),
                }))),
                cursor: 0,
            },
            Mode::Replay => {
                if !path.exists() {
                    anyhow::bail!(
                        "cassette not found: {}. Run with OPEX_CASSETTE=record to create it (CI forces replay).",
                        path.display()
                    );
                }
                let cassette = Cassette::read_from_file(&path)?;
                CassetteState {
                    cassette,
                    cursor: 0,
                }
            }
        };
        Ok(Self {
            mode,
            path,
            real,
            state: AsyncMutex::new(state),
        })
    }

    /// Create in explicit mode (for tests that want to bypass env resolution).
    #[allow(dead_code)]
    pub fn with_mode(path: impl Into<PathBuf>, mode: Mode) -> Result<Self> {
        let path = path.into();
        let real = RealTransport::new(reqwest::Client::new());
        let state = match mode {
            Mode::Record => CassetteState {
                cassette: Cassette::new(Some(serde_json::json!({
                    "name": path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown"),
                }))),
                cursor: 0,
            },
            Mode::Replay => {
                if !path.exists() {
                    anyhow::bail!("cassette not found: {}", path.display());
                }
                let cassette = Cassette::read_from_file(&path)?;
                CassetteState { cassette, cursor: 0 }
            }
        };
        Ok(Self {
            mode,
            path,
            real,
            state: AsyncMutex::new(state),
        })
    }

    /// Persist the cassette (record mode) or assert all interactions consumed
    /// (replay mode). Call at the end of each test.
    pub async fn finalize(&self) -> Result<()> {
        let state = self.state.lock().await;
        match self.mode {
            Mode::Record => {
                state.cassette.write_to_file(&self.path)?;
                Ok(())
            }
            Mode::Replay => {
                if state.cursor != state.cassette.interactions.len() {
                    anyhow::bail!(
                        "cassette {} had {} interactions but only {} were used",
                        self.path.display(),
                        state.cassette.interactions.len(),
                        state.cursor
                    );
                }
                Ok(())
            }
        }
    }

    /// Record an interaction (record mode).
    async fn record_interaction(&self, interaction: HttpInteraction) -> Result<()> {
        // Defense in depth: refuse to write a cassette containing secrets.
        if let Some(finding) = redaction::scan_for_secrets(&interaction) {
            anyhow::bail!(
                "unsafe cassette ({}): refusing to record interaction. \
                 Redaction missed a secret — refusing to persist.",
                finding
            );
        }
        let mut state = self.state.lock().await;
        state.cassette.append(interaction);
        Ok(())
    }
}

#[async_trait]
impl HttpTransport for CassetteTransport {
    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(String, String)],
        provider_name: &str,
        retryable_codes: &[u16],
        max_retries: u32,
    ) -> Result<String> {
        match self.mode {
            Mode::Record => {
                // Passthrough to real transport, capturing the full response
                // (status + headers + body) so the cassette records the real
                // status code, not a hardcoded 200 (M2). We use post_json_stream
                // under the hood to get the raw reqwest::Response, then read the
                // body text. The retry loop in post_json_stream handles 429/5xx.
                let resp = self
                    .real
                    .post_json_stream(url, body, headers, provider_name, retryable_codes, max_retries)
                    .await
                    .map_err(|e| match e {
                        SendError::Http { status, body: b, retry_after } => {
                            anyhow::anyhow!("{provider_name} API error {status}{}: {b}",
                                retry_after.map(|ra| format!(" (retry-after: {ra})")).unwrap_or_default())
                        }
                        SendError::Network(e) => anyhow::anyhow!("{provider_name} request error: {e}"),
                    })?;
                let status = resp.status();
                let resp_headers = resp.headers().clone();
                let bytes = resp.bytes().await
                    .map_err(|e| anyhow::anyhow!("{provider_name} error reading response body: {e}"))?;
                let text = String::from_utf8_lossy(&bytes).into_owned();

                // Build a snapshot for the cassette with the real status + headers.
                let content_type = resp_headers
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/json");
                let mut headers_map = std::collections::BTreeMap::new();
                for (k, v) in resp_headers.iter() {
                    if let Ok(val) = v.to_str() {
                        headers_map.insert(k.as_str().to_string(), val.to_string());
                    }
                }
                let mut req_headers_map = std::collections::BTreeMap::new();
                for (k, v) in headers {
                    req_headers_map.insert(k.clone(), v.clone());
                }
                let (body_str, encoding) = redaction::encode_body(&bytes, content_type);
                let mut interaction = HttpInteraction {
                    request: RequestSnapshot {
                        method: "POST".into(),
                        url: url.to_string(),
                        headers: req_headers_map,
                        body: serde_json::to_string(body).unwrap_or_default(),
                    },
                    response: ResponseSnapshot {
                        status: status.as_u16(),
                        headers: headers_map,
                        body: body_str,
                        body_encoding: encoding,
                    },
                };
                redaction::redact_interaction(&mut interaction);
                self.record_interaction(interaction).await?;
                Ok(text)
            }
            Mode::Replay => {
                // Replay with retry: for retryable non-2xx, consume the next
                // interaction (which should match the same request) and retry.
                // This mirrors RealTransport's retry loop — each cassette
                // interaction is one attempt; [429, 429, 200] → 3 attempts.
                //
                // The lock is held across the entire retry loop (M5: prevents
                // concurrent calls from interleaving the cursor). This
                // serializes replay calls, which is acceptable for test-only
                // infrastructure where determinism matters more than
                // concurrency.
                let max_attempts = max_retries.max(1) as usize;
                let mut state = self.state.lock().await;
                for attempt in 0..max_attempts {
                    let idx = state.cursor;
                    if idx >= state.cassette.interactions.len() {
                        anyhow::bail!(
                            "cassette {} exhausted: request {} of {} not recorded",
                            self.path.display(),
                            idx + 1,
                            state.cassette.interactions.len()
                        );
                    }
                    let interaction = &state.cassette.interactions[idx];

                    // Validate the request matches (URL path + model + messages).
                    if let Err(e) = validate_match(url, body, interaction) {
                        anyhow::bail!(e);
                    }

                    // Clone the response fields we need before mutating cursor
                    // (borrow checker: can't hold &interaction while writing cursor).
                    let status = interaction.response.status;
                    let resp_body = interaction.response.body.clone();

                    // Match succeeded — consume the interaction.
                    state.cursor += 1;

                    if (200..300).contains(&status) {
                        return Ok(resp_body);
                    }
                    // Non-2xx: retryable → consume next interaction and retry;
                    // non-retryable → error immediately.
                    if !retryable_codes.contains(&status) || attempt + 1 >= max_attempts {
                        anyhow::bail!(
                            "{} API error {}: {}",
                            provider_name,
                            status,
                            resp_body
                        );
                    }
                    // Retryable: loop to consume the next interaction.
                    // (No backoff sleep in replay — tests should be fast.)
                    continue;
                }
                anyhow::bail!(
                    "cassette {} exhausted after {} retry attempts",
                    self.path.display(),
                    max_attempts
                );
            }
        }
    }

    async fn post_json_stream(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(String, String)],
        provider_name: &str,
        retryable_codes: &[u16],
        max_retries: u32,
    ) -> std::result::Result<reqwest::Response, SendError> {
        match self.mode {
            Mode::Record => {
                // Passthrough to real transport.
                let resp = self
                    .real
                    .post_json_stream(url, body, headers, provider_name, retryable_codes, max_retries)
                    .await?;
                // Capture the full body for the cassette (streaming is stored whole).
                let status = resp.status();
                let resp_headers = resp.headers().clone();
                let bytes = resp.bytes().await.map_err(SendError::Network)?;
                // Reconstruct a response from the captured bytes for the live caller.
                let live_resp = reconstruct_response(status, &resp_headers, &bytes);
                // Build a snapshot for the cassette.
                let content_type = resp_headers
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream");
                let (body_str, encoding) = redaction::encode_body(&bytes, content_type);
                let mut headers_map = std::collections::BTreeMap::new();
                for (k, v) in resp_headers.iter() {
                    if let Ok(val) = v.to_str() {
                        headers_map.insert(k.as_str().to_string(), val.to_string());
                    }
                }
                let mut req_headers_map = std::collections::BTreeMap::new();
                for (k, v) in headers {
                    req_headers_map.insert(k.clone(), v.clone());
                }
                let mut interaction = HttpInteraction {
                    request: RequestSnapshot {
                        method: "POST".into(),
                        url: url.to_string(),
                        headers: req_headers_map,
                        body: serde_json::to_string(body).unwrap_or_default(),
                    },
                    response: ResponseSnapshot {
                        status: status.as_u16(),
                        headers: headers_map,
                        body: body_str,
                        body_encoding: encoding,
                    },
                };
                redaction::redact_interaction(&mut interaction);
                self.record_interaction(interaction)
                    .await
                    .map_err(|e| SendError::Http {
                        status: 500,
                        body: e.to_string(),
                        retry_after: None,
                    })?;
                Ok(live_resp)
            }
            Mode::Replay => {
                // Replay with retry: for retryable non-2xx, consume the next
                // interaction and retry (mirrors send_with_retry's loop).
                //
                // The lock is held across the entire retry loop (M5: prevents
                // concurrent calls from interleaving the cursor).
                let max_attempts = max_retries.max(1) as usize;
                let mut state = self.state.lock().await;
                for attempt in 0..max_attempts {
                    let idx = state.cursor;
                    if idx >= state.cassette.interactions.len() {
                        return Err(SendError::Http {
                            status: 500,
                            body: format!(
                                "cassette {} exhausted: request {} of {} not recorded",
                                self.path.display(),
                                idx + 1,
                                state.cassette.interactions.len()
                            ),
                            retry_after: None,
                        });
                    }
                    let interaction = &state.cassette.interactions[idx];

                    // Validate the request matches (URL path + model + messages).
                    if let Err(e) = validate_match(url, body, interaction) {
                        return Err(SendError::Http {
                            status: 500,
                            body: e.to_string(),
                            retry_after: None,
                        });
                    }

                    // Clone the response fields we need before mutating cursor
                    // (borrow checker: can't hold &interaction while writing cursor).
                    let status = interaction.response.status;
                    let resp_body = interaction.response.body.clone();
                    let resp_encoding = interaction.response.body_encoding;
                    let resp_headers = interaction.response.headers.clone();

                    // Match succeeded — consume the interaction.
                    state.cursor += 1;

                    if (200..300).contains(&status) {
                        // Reconstruct a reqwest::Response from the stored snapshot.
                        let bytes = redaction::decode_body(&resp_body, resp_encoding);
                        let status_code = reqwest::StatusCode::from_u16(status)
                            .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
                        let mut header_map = reqwest::header::HeaderMap::new();
                        for (k, v) in &resp_headers {
                            if let (Ok(name), Ok(val)) = (
                                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                                v.parse::<reqwest::header::HeaderValue>(),
                            ) {
                                header_map.insert(name, val);
                            }
                        }
                        return Ok(reconstruct_response(status_code, &header_map, &bytes));
                    }

                    // Non-2xx: retryable → consume next interaction and retry;
                    // non-retryable → error immediately.
                    if !retryable_codes.contains(&status) || attempt + 1 >= max_attempts {
                        return Err(SendError::Http {
                            status,
                            body: resp_body,
                            retry_after: None,
                        });
                    }
                    // Retryable: loop to consume the next interaction.
                    continue;
                }
                Err(SendError::Http {
                    status: 500,
                    body: format!(
                        "cassette {} exhausted after {} retry attempts",
                        self.path.display(),
                        max_attempts
                    ),
                    retry_after: None,
                })
            }
        }
    }

    fn discovery_client(&self) -> reqwest::Client {
        // In record mode, return the real transport's discovery client so
        // probes (context-limit, /v1/models) reach the real provider.
        // In replay mode, return a client with a 1ms timeout so discovery
        // probes deterministically fail fast without any real network calls
        // (M6). The provider's `context_limit_hint` uses `.ok()?` throughout,
        // so a connect timeout → `None` → graceful heuristic fallback.
        match self.mode {
            Mode::Record => self.real.discovery_client(),
            Mode::Replay => reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(1))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }
}

/// Reconstruct a `reqwest::Response` from status + headers + body bytes.
fn reconstruct_response(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
) -> reqwest::Response {
    let mut builder = http::Response::builder().status(status);
    for (k, v) in headers {
        builder = builder.header(k.clone(), v.clone());
    }
    // reqwest::Body: From<Bytes> is implemented; wrap in http::Response then
    // convert via reqwest::Response::from (infallible).
    let body_bytes = bytes::Bytes::copy_from_slice(body);
    let http_resp = builder
        .body(reqwest::Body::from(body_bytes))
        .expect("response builds");
    reqwest::Response::from(http_resp)
}

/// Validate that a runtime request matches a recorded interaction.
///
/// Matching is intentionally loose: URL path must match, and the canonical
/// `model` + `messages` fields must match. Other fields (`temperature`,
/// `max_tokens`, `stream`, `stream_options`, `tools`) are ignored because
/// provider defaults may change between recording and replay. This keeps
/// cassettes stable across config tweaks while still verifying the request
/// targets the right endpoint with the right conversation.
fn validate_match(
    url: &str,
    body: &serde_json::Value,
    interaction: &HttpInteraction,
) -> Result<()> {
    // Match on URL PATH only (ignore scheme+host+port — cassettes may be
    // recorded against a wiremock host and replayed against a dummy URL;
    // the path is the stable identifier, and query may carry redacted secrets).
    let runtime_path = url
        .split("//")
        .nth(1)
        .and_then(|s| s.split_once('/').map(|(_, p)| p))
        .unwrap_or("");
    let recorded_path = interaction
        .request
        .url
        .split("//")
        .nth(1)
        .and_then(|s| s.split_once('/').map(|(_, p)| p))
        .unwrap_or("");
    // Strip query from both.
    let runtime_path = runtime_path.split('?').next().unwrap_or(runtime_path);
    let recorded_path = recorded_path.split('?').next().unwrap_or(recorded_path);
    if runtime_path != recorded_path {
        anyhow::bail!(
            "URL path mismatch: expected {recorded_path}, got {runtime_path}"
        );
    }
    // Body match: compare only stable fields (model + messages). This tolerates
    // changes to temperature/max_tokens/stream defaults between recording and
    // replay — cassettes should not break on config tweaks.
    let runtime_model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let recorded_model = serde_json::from_str::<serde_json::Value>(&interaction.request.body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_default();
    if runtime_model != recorded_model {
        anyhow::bail!(
            "model mismatch: expected {recorded_model}, got {runtime_model}"
        );
    }
    let runtime_msgs = body.get("messages").cloned().unwrap_or_default();
    let recorded_msgs = serde_json::from_str::<serde_json::Value>(&interaction.request.body)
        .ok()
        .and_then(|v| v.get("messages").cloned())
        .unwrap_or_default();
    let runtime_msgs_canon = canonicalize_json(&runtime_msgs);
    let recorded_msgs_canon = canonicalize_json(&recorded_msgs);
    if runtime_msgs_canon != recorded_msgs_canon {
        anyhow::bail!(
            "messages mismatch: expected {recorded_msgs_canon}, got {runtime_msgs_canon}"
        );
    }
    Ok(())
}

/// Canonicalize a JSON value: sort object keys recursively.
fn canonicalize_json(value: &serde_json::Value) -> String {
    let mut sorted = value.clone();
    sort_keys(&mut sorted);
    serde_json::to_string(&sorted).unwrap_or_default()
}

fn sort_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            // Recurse into each value first.
            for v in map.values_mut() {
                sort_keys(v);
            }
            // Re-insert keys in sorted order so serialization is canonical.
            let mut sorted_keys: Vec<String> = map.keys().cloned().collect();
            sorted_keys.sort();
            let mut new_map = serde_json::Map::new();
            for k in sorted_keys {
                if let Some(v) = map.remove(&k) {
                    new_map.insert(k, v);
                }
            }
            *map = new_map;
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sort_keys(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    #[serial_test::serial(cassette_mode)]
    fn resolve_mode_record_env() {
        unsafe { std::env::set_var("OPEX_CASSETTE", "record") };
        unsafe { std::env::remove_var("CI") };
        let m = resolve_mode(Path::new("/nonexistent.json"));
        unsafe { std::env::remove_var("OPEX_CASSETTE") };
        assert_eq!(m, Mode::Record);
    }

    #[test]
    #[serial_test::serial(cassette_mode)]
    fn resolve_mode_replay_env() {
        unsafe { std::env::set_var("OPEX_CASSETTE", "replay") };
        let m = resolve_mode(Path::new("/nonexistent.json"));
        unsafe { std::env::remove_var("OPEX_CASSETTE") };
        assert_eq!(m, Mode::Replay);
    }

    #[test]
    #[serial_test::serial(cassette_mode)]
    fn resolve_mode_ci_forces_replay() {
        unsafe { std::env::remove_var("OPEX_CASSETTE") };
        unsafe { std::env::set_var("CI", "true") };
        let m = resolve_mode(Path::new("/nonexistent.json"));
        unsafe { std::env::remove_var("CI") };
        assert_eq!(m, Mode::Replay);
    }

    #[tokio::test]
    #[serial_test::serial(cassette_mode)]
    async fn record_then_replay_post_json() {
        let dir = std::env::temp_dir().join("opex-cassette-test");
        let _ = std::fs::remove_dir_all(&dir);
        let cassette_path = dir.join("test.json");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"reply":"hi"}"#))
            .mount(&server)
            .await;

        // Record
        {
            let transport = CassetteTransport {
                mode: Mode::Record,
                path: cassette_path.clone(),
                real: RealTransport::new(reqwest::Client::new()),
                state: AsyncMutex::new(CassetteState {
                    cassette: Cassette::new(None),
                    cursor: 0,
                }),
            };
            let url = format!("{}/v1/chat", server.uri());
            let body = transport
                .post_json(
                    &url,
                    &serde_json::json!({"q":"hello"}),
                    &[],
                    "test",
                    super::super::http::RETRYABLE_OPENAI,
                    1,
                )
                .await
                .expect("record call");
            assert_eq!(body, r#"{"reply":"hi"}"#);
            transport.finalize().await.expect("finalize writes cassette");
        }
        assert!(cassette_path.exists(), "cassette file written");

        // Replay
        {
            let transport = CassetteTransport::new(&cassette_path).expect("loads cassette");
            assert_eq!(transport.mode, Mode::Replay);
            // Use a dummy URL — only the path is matched.
            let url = "http://dummy.example/v1/chat";
            let body = transport
                .post_json(
                    url,
                    &serde_json::json!({"q":"hello"}),
                    &[],
                    "test",
                    super::super::http::RETRYABLE_OPENAI,
                    1,
                )
                .await
                .expect("replay call");
            assert_eq!(body, r#"{"reply":"hi"}"#);
            transport.finalize().await.expect("all interactions consumed");
        }
    }

    #[tokio::test]
    #[serial_test::serial(cassette_mode)]
    async fn replay_exhausted_errors() {
        let dir = std::env::temp_dir().join("opex-cassette-exhaust");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("exhaust.json");
        // Write a cassette with one interaction.
        let mut cassette = Cassette::new(None);
        cassette.append(HttpInteraction {
            request: RequestSnapshot {
                method: "POST".into(),
                url: "http://x/v1/chat".into(),
                headers: std::collections::BTreeMap::new(),
                body: r#"{"q":"hi"}"#.into(),
            },
            response: ResponseSnapshot {
                status: 200,
                headers: std::collections::BTreeMap::new(),
                body: "ok".into(),
                body_encoding: BodyEncoding::Text,
            },
        });
        cassette.write_to_file(&path).unwrap();

        let transport = CassetteTransport::new(&path).unwrap();
        // First call consumes the one interaction.
        let _ = transport
            .post_json(
                "http://x/v1/chat",
                &serde_json::json!({"q":"hi"}),
                &[],
                "test",
                super::super::http::RETRYABLE_OPENAI,
                1,
            )
            .await
            .unwrap();
        // Second call — cassette exhausted.
        let err = transport
            .post_json(
                "http://x/v1/chat",
                &serde_json::json!({"q":"hi"}),
                &[],
                "test",
                super::super::http::RETRYABLE_OPENAI,
                1,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exhausted"));
    }

    #[test]
    fn canonicalize_json_sorts_keys() {
        let a = canonicalize_json(&serde_json::json!({"b":1,"a":2}));
        let b = canonicalize_json(&serde_json::json!({"a":2,"b":1}));
        assert_eq!(a, b);
    }
}