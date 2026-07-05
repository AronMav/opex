//! HTTP transport seam for LLM provider calls.
//!
//! All LLM-bound HTTP traffic flows through [`HttpTransport`]. The production
//! implementation [`RealTransport`] wraps a [`reqwest::Client`] 1:1 and reuses
//! the retry/backoff helpers in [`super::http`]. A second implementation
//! (`CassetteTransport`, added in a follow-up) records/replays wire traffic
//! for offline provider tests — see `cassette.rs`.
//!
//! Design notes:
//! - The seam sits *below* `retry_http_post` / `send_with_retry` (those helpers
//!   stay unchanged and are called by `RealTransport`). `stream_with_cancellation`
//!   stays *above* the seam in each provider — it wraps `resp.bytes_stream()`
//!   exactly as before.
//! - Auth headers are passed explicitly as `&[(String, String)]` instead of via
//!   a `RequestBuilder` closure. This keeps the trait reqwest-`RequestBuilder`-
//!   free at the call boundary (the closure only ever added auth headers).
//! - `post_json_stream` returns a [`reqwest::Response`] (body unconsumed) so the
//!   caller can feed `resp.bytes_stream()` into `stream_with_cancellation`.
//! - `discovery_client()` exposes a cheap-cloned [`reqwest::Client`] for
//!   incidental non-LLM calls (e.g. OpenAI `/v1/models` context-limit probes).
//!   `CassetteTransport` returns a default client — those probes are
//!   best-effort and naturally no-op offline.

use anyhow::Result;
use async_trait::async_trait;

use super::http::{SendError, retry_http_post_custom, send_with_retry};

/// Transport seam for LLM provider HTTP calls.
///
/// Two methods cover the two call shapes in the codebase:
/// - [`HttpTransport::post_json`] — non-streaming; returns the full decoded body.
/// - [`HttpTransport::post_json_stream`] — streaming; returns the raw
///   [`reqwest::Response`] (body unconsumed) so the caller can wrap it with
///   `stream_with_cancellation`.
///
/// Both methods run the shared retry/backoff loop (see [`super::http`]) and
/// classify failures into [`SendError`].
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// Non-streaming POST with retry + 2-attempt body-read retry.
    ///
    /// `headers` is applied to every (re)try attempt (auth, provider-specific
    /// headers like `anthropic-version`). Pass an empty slice when no extra
    /// headers are needed (e.g. Google, which carries the key in the URL).
    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(String, String)],
        provider_name: &str,
        retryable_codes: &[u16],
        max_retries: u32,
    ) -> Result<String>;

    /// Streaming POST with retry. Returns the raw [`reqwest::Response`] (body
    /// unconsumed). The caller is responsible for `resp.bytes_stream()` +
    /// `stream_with_cancellation` (unchanged, above the seam).
    async fn post_json_stream(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(String, String)],
        provider_name: &str,
        retryable_codes: &[u16],
        max_retries: u32,
    ) -> std::result::Result<reqwest::Response, SendError>;

    /// A cheap-cloned [`reqwest::Client`] for incidental non-LLM calls
    /// (context-limit discovery, model probes). `reqwest::Client` is `Clone`
    /// (it is `Arc` internally), so this is allocation-cheap.
    ///
    /// `CassetteTransport` returns a default client — discovery probes are
    /// best-effort and naturally no-op offline.
    fn discovery_client(&self) -> reqwest::Client;
}

/// Production transport: wraps a [`reqwest::Client`] 1:1 and reuses the shared
/// retry/backoff helpers in [`super::http`].
pub struct RealTransport {
    client: reqwest::Client,
}

impl RealTransport {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Apply a header list to a [`reqwest::RequestBuilder`].
    fn apply_headers(
        mut req: reqwest::RequestBuilder,
        headers: &[(String, String)],
    ) -> reqwest::RequestBuilder {
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req
    }
}

#[async_trait]
impl HttpTransport for RealTransport {
    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(String, String)],
        provider_name: &str,
        retryable_codes: &[u16],
        max_retries: u32,
    ) -> Result<String> {
        // Capture headers by value so the closure is `Fn` (reusable across the
        // internal retry loop's attempts — `retry_http_post_custom` takes `Fn`).
        let headers = headers.to_vec();
        retry_http_post_custom(
            &self.client,
            url,
            body,
            provider_name,
            retryable_codes,
            max_retries,
            move |req| Self::apply_headers(req, &headers),
        )
        .await
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
        // `send_with_retry` takes `FnMut` — a move closure capturing headers by
        // value works.
        let headers = headers.to_vec();
        send_with_retry(
            &self.client,
            url,
            body,
            provider_name,
            retryable_codes,
            max_retries,
            move |req| Self::apply_headers(req, &headers),
        )
        .await
    }

    fn discovery_client(&self) -> reqwest::Client {
        self.client.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DEFAULT_RETRIES: u32 = 3;

    /// RealTransport.post_json_stream retries 503 → 200 (mirrors the
    /// `send_with_retry` test in http.rs but through the trait).
    #[tokio::test(start_paused = true)]
    async fn post_json_stream_retries_503_and_succeeds() {
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

        let transport = RealTransport::new(reqwest::Client::new());
        let url = format!("{}/v1/chat/completions", server.uri());
        let resp = transport
            .post_json_stream(
                &url,
                &serde_json::json!({}),
                &[],
                "test",
                super::super::http::RETRYABLE_OPENAI,
                DEFAULT_RETRIES,
            )
            .await
            .expect("should succeed after retry");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    /// post_json applies headers (auth) on every attempt.
    #[tokio::test(start_paused = true)]
    async fn post_json_applies_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(wiremock::matchers::header("x-api-key", "secret"))
            .and(wiremock::matchers::header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;

        let transport = RealTransport::new(reqwest::Client::new());
        let url = format!("{}/v1/messages", server.uri());
        let body = transport
            .post_json(
                &url,
                &serde_json::json!({}),
                &[
                    ("x-api-key".to_string(), "secret".to_string()),
                    ("anthropic-version".to_string(), "2023-06-01".to_string()),
                ],
                "anthropic",
                super::super::http::RETRYABLE_ANTHROPIC,
                DEFAULT_RETRIES,
            )
            .await
            .expect("should succeed");
        assert_eq!(body, r#"{"ok":true}"#);
    }
}