//! ToolgateClient — HTTP transport for /v1/embeddings and /health.

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::StatusCode;
use serde_json::Value;

use crate::retry::{with_retry, RetryPolicy, RetryableError};
use crate::trace::inject_trace_context;

#[derive(Debug, Clone)]
pub struct ToolgateHealth {
    pub active_embedding_provider: Option<String>,
    pub raw: Value,
}

#[derive(Clone)]
pub struct ToolgateClient {
    http: reqwest::Client,
    base_url: String,
    retry_policy: RetryPolicy,
    requested_dimensions: u32,
}

impl ToolgateClient {
    /// `base_url` example: `"http://localhost:9011"` (без `/v1`).
    /// Пустой `base_url` → клиент считается non-configured (`is_configured()==false`).
    pub fn new(base_url: impl Into<String>, requested_dimensions: u32) -> Self {
        let http = reqwest::Client::builder()
            // 60s tolerates cold-start of CPU-only embedding models on Pi/ARM64.
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            retry_policy: RetryPolicy::default(),
            requested_dimensions,
        }
    }

    pub fn with_retry(self, policy: RetryPolicy) -> Self {
        Self { retry_policy: policy, ..self }
    }

    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty()
    }

    /// Determine embedding dimension by sending a single probe request.
    /// Применяет retry_policy.
    pub async fn probe_dim(&self) -> Result<u32> {
        if !self.is_configured() {
            anyhow::bail!("toolgate client not configured");
        }
        let url = format!("{}/v1/embeddings", self.base_url);
        let mut body = serde_json::json!({ "input": "dimension probe" });
        if self.requested_dimensions > 0 {
            body["dimensions"] = serde_json::json!(self.requested_dimensions);
        }
        let policy = self.retry_policy;
        with_retry(&policy, "probe_dim", || async {
            let req = self.http.post(&url).json(&body);
            let req = inject_trace_context(req);
            let resp = req.send().await.map_err(classify_reqwest_err)?;
            let status = resp.status();
            if !status.is_success() {
                return Err(classify_status(status, "probe_dim"));
            }
            let body: Value = resp.json().await.map_err(|e| {
                RetryableError::Permanent(anyhow!("failed to parse probe response: {e}"))
            })?;
            let vec_len = body["data"][0]["embedding"]
                .as_array()
                .ok_or_else(|| {
                    RetryableError::Permanent(anyhow!("missing data[0].embedding in probe response"))
                })?
                .len();
            if vec_len == 0 {
                return Err(RetryableError::Permanent(anyhow!(
                    "probe returned empty embedding vector"
                )));
            }
            Ok(vec_len as u32)
        })
        .await
    }
}

fn classify_reqwest_err(e: reqwest::Error) -> RetryableError {
    if e.is_timeout() || e.is_connect() {
        RetryableError::Transient(anyhow!("network error: {e}"))
    } else {
        RetryableError::Permanent(anyhow!("non-retryable error: {e}"))
    }
}

fn classify_status(status: StatusCode, op: &str) -> RetryableError {
    let msg = anyhow!("{op} returned HTTP {status}");
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        RetryableError::Transient(msg)
    } else {
        RetryableError::Permanent(msg)
    }
}
