//! Shared HTTP utilities for LLM providers: retry loop.

use anyhow::Result;
use rand::Rng;
use std::time::Duration;

/// Configurable backoff policy for HTTP retries.
pub struct BackoffPolicy {
    pub base: Duration,
    pub factor: f64,
    pub max_delay: Duration,
    pub jitter: Duration,
    pub max_retries: u32,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            factor: 3.0,
            max_delay: Duration::from_secs(30),
            jitter: Duration::from_millis(500),
            max_retries: 3,
        }
    }
}

impl BackoffPolicy {
    fn delay(&self, attempt: u32) -> Duration {
        let exp = self.base.as_millis() as f64 * self.factor.powi(attempt as i32);
        let capped = exp.min(self.max_delay.as_millis() as f64) as u64;
        let jitter_ms = if self.jitter.as_millis() > 0 {
            rand::rng().random_range(0..self.jitter.as_millis() as u64)
        } else {
            0
        };
        Duration::from_millis(capped + jitter_ms)
    }
}

/// Retry an HTTP POST request with exponential backoff + jitter.
///
/// Retained as a low-level helper for direct `reqwest::Client` callers that
/// don't use the [`HttpTransport`](super::transport::HttpTransport) seam (e.g.
/// ad-hoc internal probes). Provider call paths now go through
/// `RealTransport::post_json` instead.
#[allow(dead_code)]
pub async fn retry_http_post(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    api_key: &str,
    provider_name: &str,
    retryable_codes: &[u16],
    max_retries: u32,
) -> Result<String> {
    retry_http_post_custom(client, url, body, provider_name, retryable_codes, max_retries, |req| {
        if api_key.is_empty() {
            req
        } else {
            req.bearer_auth(api_key)
        }
    }).await
}

/// Like [`retry_http_post`] but accepts a closure to customize each request
/// (e.g. add custom auth headers). The closure receives a `RequestBuilder`
/// that already has URL and JSON body set, and must return the builder.
///
/// Also retries once if the response body fails to decode — this can happen
/// when two concurrent subagents hit a thinking-mode LLM (DeepSeek) and the
/// server closes the connection before the large reasoning_content is fully sent.
pub async fn retry_http_post_custom(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    provider_name: &str,
    retryable_codes: &[u16],
    max_retries: u32,
    customize: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<String> {
    for body_attempt in 0..2u32 {
        let resp = send_with_retry(client, url, body, provider_name, retryable_codes, max_retries, &customize)
            .await
            .map_err(|e| match e {
                SendError::Http { status, body: b, .. } =>
                    anyhow::anyhow!("{provider_name} API error {status}: {b}"),
                SendError::Network(e) =>
                    anyhow::anyhow!("{provider_name} request error: {e}"),
            })?;
        match resp.text().await {
            Ok(text) => return Ok(text),
            Err(e) if body_attempt == 0 => {
                tracing::warn!(
                    provider = %provider_name,
                    error = %e,
                    "response body read failed, retrying request"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(anyhow::anyhow!("{provider_name} error reading response body: {e}")),
        }
    }
    unreachable!()
}

/// Standard retryable HTTP status codes for OpenAI-compatible providers.
pub const RETRYABLE_OPENAI: &[u16] = &[429, 500, 502, 503];

/// Retryable codes for Anthropic (includes 529 overloaded).
pub const RETRYABLE_ANTHROPIC: &[u16] = &[429, 500, 502, 503, 529];

/// Parse an HTTP `Retry-After` header value (RFC 7231 §7.1.3) into a Duration.
/// Accepts both delta-seconds (e.g. "60") and HTTP-date (best-effort, falls
/// back to a default if unparseable). Caps at 5 minutes to avoid stalling.
fn parse_retry_after(value: &str) -> Duration {
    // Try delta-seconds first.
    if let Ok(secs) = value.trim().parse::<u64>() {
        return Duration::from_secs(secs.min(300));
    }
    // HTTP-date is rarely used by LLM providers; fall back to a default 60s.
    // (Parsing HTTP-date requires chrono's `DateTime::parse_from_rfc2822`,
    // which would add a parse dependency for a marginal case.)
    Duration::from_secs(60)
}

/// Typed error returned by [`send_with_retry`].
#[derive(Debug)]
pub enum SendError {
    /// Non-2xx HTTP response after all retry attempts, or a 400 (no retry).
    /// `retry_after` carries the `Retry-After` header value when present (e.g.
    /// for 429 responses), so callers can surface it in error messages.
    Http { status: u16, body: String, retry_after: Option<String> },
    /// Network / connection failure after all retry attempts.
    Network(reqwest::Error),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Http { status, body, retry_after } => {
                if let Some(ra) = retry_after {
                    write!(f, "HTTP {status} (retry-after: {ra}): {body}")
                } else {
                    write!(f, "HTTP {status}: {body}")
                }
            }
            SendError::Network(e) => write!(f, "network error: {e}"),
        }
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SendError::Network(e) => Some(e),
            SendError::Http { .. } => None,
        }
    }
}

/// Low-level HTTP POST with exponential backoff retry.
///
/// Returns the raw `reqwest::Response` on success (body not yet consumed).
/// Callers decide how to read the body (text for non-streaming, stream for SSE).
///
/// Error classification:
/// - 400 → logged + `SendError::Http { status: 400 }` immediately (no retry)
/// - retryable codes (e.g. 429/500/502/503) → retried up to `max_retries - 1` times
/// - other non-2xx → `SendError::Http` immediately
/// - network error → retried; final attempt → `SendError::Network`
#[tracing::instrument(
    name = "llm.request",
    skip_all,
    fields(
        provider = %provider_name,
        // host / path of the upstream URL — useful for distinguishing
        // routed providers (e.g. ollama vs openai-compatible proxy)
        // when the same provider_name covers multiple endpoints.
        http.host = tracing::field::Empty,
        http.url_path = tracing::field::Empty,
        // Request body size in bytes — proxy of request "weight"
        // (large message history, attached tools schema). Recorded
        // up front so the span carries it even on early-exit errors.
        http.request_size_bytes = tracing::field::Empty,
        // Final response status + retry count — recorded at exit.
        http.status_code = tracing::field::Empty,
        retry_attempts = tracing::field::Empty,
    ),
)]
// reviewed: preview slice bounded by is_char_boundary walk-back — char boundary
#[allow(clippy::string_slice)]
pub async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    provider_name: &str,
    retryable_codes: &[u16],
    max_retries: u32,
    mut customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, SendError> {
    // Populate static span fields. Parsing the URL once here is
    // cheap and the result is the same across all retry attempts
    // for a given call, so we don't need to re-record per attempt.
    {
        let span = tracing::Span::current();
        if let Ok(parsed) = reqwest::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                span.record("http.host", tracing::field::display(host));
            }
            span.record("http.url_path", tracing::field::display(parsed.path()));
        }
        // serde_json::to_vec is the closest cheap proxy for the on-wire
        // request size; reqwest's actual JSON serialization may differ
        // by a handful of bytes (whitespace, key order) but this is
        // accurate enough for cost/latency dashboards.
        if let Ok(bytes) = serde_json::to_vec(body) {
            span.record("http.request_size_bytes", bytes.len() as u64);
        }
    }
    let policy = BackoffPolicy { max_retries, ..Default::default() };

    for attempt in 0..policy.max_retries {
        let start = std::time::Instant::now();
        let req = customize(client.post(url).json(body));

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                tracing::info!(
                    provider = %provider_name,
                    status = %status,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    attempt,
                    "LLM API responded"
                );

                if status.is_success() {
                    let span = tracing::Span::current();
                    span.record("http.status_code", status.as_u16());
                    span.record("retry_attempts", attempt);
                    return Ok(resp);
                }

                let code = status.as_u16();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(String::from);
                let err_text = resp.text().await.unwrap_or_default();

                if code == 400 {
                    let body_preview = serde_json::to_string(body).unwrap_or_default();
                    let mut end = body_preview.len().min(4000);
                    while end > 0 && !body_preview.is_char_boundary(end) { end -= 1; }
                    tracing::error!(
                        provider = %provider_name,
                        request_body = %&body_preview[..end],
                        "400 Bad Request — dumping request body for diagnosis"
                    );
                    let span = tracing::Span::current();
                    span.record("http.status_code", code);
                    span.record("retry_attempts", attempt);
                    return Err(SendError::Http { status: code, body: err_text, retry_after });
                }

                if !retryable_codes.contains(&code) || attempt == policy.max_retries - 1 {
                    let span = tracing::Span::current();
                    span.record("http.status_code", code);
                    span.record("retry_attempts", attempt);
                    return Err(SendError::Http { status: code, body: err_text, retry_after });
                }

                // Honor the server's retry-after header if present (RFC 7231
                // §7.1.3). Use the larger of the policy backoff and the
                // retry-after delay so we don't retry before the server is ready.
                let backoff = if let Some(ref ra) = retry_after {
                    let ra_dur = parse_retry_after(ra);
                    let policy_dur = policy.delay(attempt);
                    if ra_dur > policy_dur {
                        tracing::warn!(
                            provider = %provider_name,
                            status = %status,
                            attempt,
                            retry_after = %ra,
                            backoff_ms = ra_dur.as_millis() as u64,
                            "retrying LLM request (honoring server retry-after)"
                        );
                        ra_dur
                    } else {
                        tracing::warn!(
                            provider = %provider_name,
                            status = %status,
                            attempt,
                            backoff_ms = policy_dur.as_millis() as u64,
                            "retrying LLM request"
                        );
                        policy_dur
                    }
                } else {
                    let backoff = policy.delay(attempt);
                    tracing::warn!(
                        provider = %provider_name,
                        status = %status,
                        attempt,
                        backoff_ms = backoff.as_millis() as u64,
                        "retrying LLM request"
                    );
                    backoff
                };
                tokio::time::sleep(backoff).await;
            }
            Err(e) => {
                tracing::warn!(
                    provider = %provider_name,
                    error = %e,
                    attempt,
                    "LLM request failed"
                );

                if attempt == policy.max_retries - 1 {
                    tracing::Span::current().record("retry_attempts", attempt);
                    return Err(SendError::Network(e));
                }

                tokio::time::sleep(policy.delay(attempt)).await;
            }
        }
    }

    unreachable!("loop always returns on the final attempt")
}


#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DEFAULT_RETRIES: u32 = 3;

    /// 503 twice → 200: send_with_retry should succeed on the third attempt.
    /// Uses tokio time-pause so backoff sleeps are instant.
    #[tokio::test(start_paused = true)]
    async fn send_with_retry_retries_503_and_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/v1/chat/completions", server.uri());
        let result = send_with_retry(
            &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, DEFAULT_RETRIES, |r| r,
        ).await;
        assert!(result.is_ok(), "expected Ok after retry, got {:?}", result.err());
        assert_eq!(server.received_requests().await.unwrap().len(), 3); // 2 failures + 1 success
    }

    /// 503 three times (all retries exhausted): should return SendError::Http { status: 503 }.
    #[tokio::test(start_paused = true)]
    async fn send_with_retry_fails_after_all_503s() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/v1/chat/completions", server.uri());
        let result = send_with_retry(
            &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, DEFAULT_RETRIES, |r| r,
        ).await;
        assert!(
            matches!(result, Err(SendError::Http { status: 503, .. })),
            "expected Http(503), got {:?}", result
        );
        assert_eq!(server.received_requests().await.unwrap().len(), DEFAULT_RETRIES as usize);
    }

    /// 400 should not be retried and returns immediately.
    #[tokio::test]
    async fn send_with_retry_no_retry_on_400() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/v1/chat/completions", server.uri());
        let result = send_with_retry(
            &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, DEFAULT_RETRIES, |r| r,
        ).await;
        assert!(
            matches!(result, Err(SendError::Http { status: 400, .. })),
            "expected Http(400), got {:?}", result
        );
        // exactly 1 request — no retry on 400
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
