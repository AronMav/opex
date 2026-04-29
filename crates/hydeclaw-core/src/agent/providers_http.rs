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
                SendError::Http { status, body: b } =>
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

/// Typed error returned by [`send_with_retry`].
#[derive(Debug)]
pub enum SendError {
    /// Non-2xx HTTP response after all retry attempts, or a 400 (no retry).
    Http { status: u16, body: String },
    /// Network / connection failure after all retry attempts.
    Network(reqwest::Error),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
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
pub async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    provider_name: &str,
    retryable_codes: &[u16],
    max_retries: u32,
    mut customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, SendError> {
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
                    return Ok(resp);
                }

                let code = status.as_u16();
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
                    return Err(SendError::Http { status: code, body: err_text });
                }

                if !retryable_codes.contains(&code) || attempt == policy.max_retries - 1 {
                    return Err(SendError::Http { status: code, body: err_text });
                }

                let backoff = policy.delay(attempt);
                tracing::warn!(
                    provider = %provider_name,
                    status = %status,
                    attempt,
                    backoff_ms = backoff.as_millis() as u64,
                    "retrying LLM request"
                );
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
