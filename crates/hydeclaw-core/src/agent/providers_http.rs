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
pub async fn retry_http_post_custom(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    provider_name: &str,
    retryable_codes: &[u16],
    mut customize: impl FnMut(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<String> {
    let policy = BackoffPolicy::default();
    let mut last_error = String::new();

    for attempt in 0..policy.max_retries {
        let start = std::time::Instant::now();
        let req = client.post(url).json(body);
        let req = customize(req);

        let resp_result = req.send().await;
        let elapsed = start.elapsed();

        match resp_result {
            Ok(resp) => {
                let status = resp.status();
                tracing::info!(
                    provider = %provider_name,
                    status = %status,
                    elapsed_ms = elapsed.as_millis() as u64,
                    attempt,
                    "LLM API responded"
                );

                if status.is_success() {
                    return Ok(resp.text().await?);
                }

                let err_text = resp.text().await.unwrap_or_default();
                last_error = format!("{provider_name} API error {status}: {err_text}");

                if status.as_u16() == 400 {
                    let body_preview = serde_json::to_string(body).unwrap_or_default();
                    let mut end = body_preview.len().min(4000);
                    while end > 0 && !body_preview.is_char_boundary(end) { end -= 1; }
                    let truncated = &body_preview[..end];
                    tracing::error!(
                        provider = %provider_name,
                        request_body = %truncated,
                        "400 Bad Request — dumping request body for diagnosis"
                    );
                }

                let retryable = retryable_codes.contains(&status.as_u16());
                if !retryable || attempt == policy.max_retries - 1 {
                    anyhow::bail!("{last_error}");
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
                last_error = format!("{provider_name} request error: {e}");
                tracing::warn!(
                    provider = %provider_name,
                    error = %e,
                    attempt,
                    "LLM request failed"
                );

                if attempt == policy.max_retries - 1 {
                    anyhow::bail!("{last_error}");
                }

                tokio::time::sleep(policy.delay(attempt)).await;
            }
        }
    }

    if !last_error.is_empty() {
        anyhow::bail!("{last_error}");
    }
    anyhow::bail!("{provider_name} request failed after all retries")
}

/// Standard retryable HTTP status codes for OpenAI-compatible providers.
pub const RETRYABLE_OPENAI: &[u16] = &[429, 500, 502, 503];

/// Retryable codes for Anthropic (includes 529 overloaded).
pub const RETRYABLE_ANTHROPIC: &[u16] = &[429, 500, 502, 503, 529];

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
