//! Shared HTTP utilities for LLM providers: retry loop, SSE parsing.

use anyhow::Result;
use rand::Rng;
use std::time::Duration;
use tokio::sync::mpsc;

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
) -> Result<String> {
    retry_http_post_custom(client, url, body, provider_name, retryable_codes, |req| {
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
    customize: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<String> {
    for body_attempt in 0..2u32 {
        let resp = send_with_retry(client, url, body, provider_name, retryable_codes, |req| customize(req))
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
    mut customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, SendError> {
    let policy = BackoffPolicy::default();

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

/// Parse an SSE byte stream with cooperative cancellation + inactivity/max-duration timers.
///
/// The `cancel` token and `timeouts` are threaded into `stream_with_cancellation`,
/// which backs the returned byte stream with a producer task that enforces
/// `stream_inactivity_secs` / `stream_max_duration_secs` and respects token
/// cancellation. On cancellation the returned error is a typed `LlmCallError`
/// wrapped in `anyhow::Error` — callers can `downcast_ref::<LlmCallError>()`
/// to classify (see `LlmCallError::is_failover_worthy`).
///
/// Note: this helper currently has no active runtime callers — provider
/// integrations inlined their own copies of this loop for maximum control
/// over per-chunk accumulation (see `providers_openai.rs`, `providers_anthropic.rs`,
/// `providers_google.rs`). Kept available for a future generic SSE path.
#[allow(dead_code)]
pub async fn parse_sse_stream(
    resp: reqwest::Response,
    chunk_tx: &mpsc::UnboundedSender<String>,
    mut on_data: impl FnMut(&str, &mut crate::agent::thinking::ThinkingFilter, &mpsc::UnboundedSender<String>) -> SseAction,
    cancel: tokio_util::sync::CancellationToken,
    timeouts: crate::agent::providers::TimeoutsConfig,
    provider_name: &str,
) -> Result<()> {
    let mut buffer = String::new();
    let mut thinking_filter = crate::agent::thinking::ThinkingFilter::new();
    let mut partial_text = String::new();

    use tokio_stream::StreamExt;
    use crate::agent::providers::{CancelSlot, LlmCallError, cancellable_stream::stream_with_cancellation};

    let slot = CancelSlot::new();
    // TODO: use cancel.child_token() here for retry isolation once this function has active callers.
    let byte_stream = stream_with_cancellation(
        resp.bytes_stream(),
        cancel.clone(),
        slot.clone(),
        timeouts,
    );
    let mut byte_stream = std::pin::pin!(byte_stream);
    while let Some(chunk_result) = StreamExt::next(&mut byte_stream).await {
        let chunk_bytes = match chunk_result {
            Ok(b) => b,
            Err(e) => {
                return Err(anyhow::Error::new(LlmCallError::from(e)));
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));
        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    return Ok(());
                }
                // Tee the payload into `partial_text` so cancellation errors
                // can surface whatever the caller has already observed. The
                // `on_data` handler still drives per-line accumulation into
                // `chunk_tx` as before.
                partial_text.push_str(data);
                match on_data(data, &mut thinking_filter, chunk_tx) {
                    SseAction::Continue => {}
                    SseAction::Done => return Ok(()),
                }
            }
        }
    }

    // Stream exited. If cancellation fired, surface the typed reason.
    if let Some(reason) = slot.get() {
        use crate::agent::providers::error::{CancelReason, PartialState};
        let partial_state = if !partial_text.is_empty() {
            PartialState::Text(partial_text.clone())
        } else {
            PartialState::Empty
        };
        let err = match reason {
            CancelReason::InactivityTimeout { silent_secs } => LlmCallError::InactivityTimeout {
                provider: provider_name.to_string(),
                silent_secs,
                partial_state,
            },
            CancelReason::MaxDurationExceeded { elapsed_secs } => LlmCallError::MaxDurationExceeded {
                provider: provider_name.to_string(),
                elapsed_secs,
                partial_state,
            },
            CancelReason::UserCancelled => LlmCallError::UserCancelled { partial_state },
            CancelReason::ShutdownDrain => LlmCallError::ShutdownDrain { partial_state },
        };
        return Err(anyhow::Error::new(err));
    }

    Ok(())
}

/// Control flow from SSE data handler.
#[allow(dead_code)]
pub enum SseAction {
    Continue,
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
            &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, |r| r,
        ).await;
        assert!(result.is_ok(), "expected Ok after retry, got {:?}", result.err());
        assert_eq!(server.received_requests().await.unwrap().len(), 3); // 3 = BackoffPolicy::default().max_retries
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
            &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, |r| r,
        ).await;
        assert!(
            matches!(result, Err(SendError::Http { status: 503, .. })),
            "expected Http(503), got {:?}", result
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 3); // 3 = BackoffPolicy::default().max_retries
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
            &client, &url, &serde_json::json!({}), "test", RETRYABLE_OPENAI, |r| r,
        ).await;
        assert!(
            matches!(result, Err(SendError::Http { status: 400, .. })),
            "expected Http(400), got {:?}", result
        );
        // exactly 1 request — no retry on 400
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
