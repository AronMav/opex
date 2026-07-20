//! ToolgateClient — HTTP transport for /v1/embeddings and /health.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
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
    ///
    /// Connection-pool hygiene (2026-07-20): `pool_max_idle_per_host(0)` disables
    /// keep-alive pooling entirely — every request opens a fresh TCP connection.
    /// This is deliberate: after a toolgate restart the old connections in the
    /// pool are dead (server-side closed), and reqwest's default pool has no
    /// mechanism to detect this before sending. Each retry would reuse the same
    /// dead connection, wait the full `timeout`, then retry — 3×timeout of
    /// silence per chat turn. With pooling disabled, a dead toolgate fails
    /// fast at `connect_timeout` (5s), and each retry opens a fresh connection
    /// that either succeeds immediately or fails quickly. Embedding calls are
    /// infrequent enough (1–2 per turn) that connection reuse provides no
    /// meaningful throughput gain.
    ///
    /// `timeout(8s)` — embedding is a fast operation (1–2s normally). 8s is
    /// generous enough for a slow provider round-trip but short enough that 2
    /// retries (16s worst case) don't block bootstrap indefinitely. The
    /// bootstrap context builder additionally wraps embedding-dependent
    /// enhancements in an 8s fail-soft timeout, so a hung provider degrades
    /// gracefully rather than stalling the agent's reply.
    ///
    /// `pool_max_idle_per_host(8)` — keep up to 8 idle connections to toolgate
    /// for reuse. This avoids the "connection refused" storm when multiple
    /// embedding calls hit toolgate concurrently and each opens a fresh TCP
    /// connection. The original `pool_max_idle_per_host(0)` (no reuse) caused
    /// transient connection failures under concurrent load.
    pub fn new(base_url: impl Into<String>, requested_dimensions: u32) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .pool_max_idle_per_host(8)
            .tcp_keepalive(Duration::from_secs(15))
            .build()
            .expect("failed to build embedding HTTP client: invalid timeout/pool configuration");
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

    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let vs = self.embed_inner(&[text]).await?;
        vs.into_iter()
            .next()
            .ok_or_else(|| anyhow!("empty result"))
    }

    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        self.embed_inner(texts).await
    }

    /// Fetch /health. НЕ применяет retry — failure возвращается немедленно,
    /// caller сам решает, как ждать.
    pub async fn fetch_health(&self) -> Result<ToolgateHealth> {
        if !self.is_configured() {
            anyhow::bail!("toolgate client not configured");
        }
        let url = format!("{}/health", self.base_url);
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("toolgate health request failed")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("toolgate /health returned HTTP {status}");
        }
        let raw: Value = resp
            .json()
            .await
            .context("failed to parse /health response")?;
        let active = raw["active_providers"]["embedding"]
            .as_str()
            .map(|s| s.to_string());
        Ok(ToolgateHealth {
            active_embedding_provider: active,
            raw,
        })
    }

    async fn embed_inner(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if !self.is_configured() {
            anyhow::bail!("toolgate client not configured");
        }
        let url = format!("{}/v1/embeddings", self.base_url);
        // Toolgate всегда резолвит активную модель из своего registry — поле
        // `model` в body не отправляем. requested_dimensions передаём только
        // если задано явно (актуально для OpenAI text-embedding-3-* с MRL).
        let mut body = if texts.len() == 1 {
            serde_json::json!({ "input": texts[0] })
        } else {
            serde_json::json!({ "input": texts })
        };
        if self.requested_dimensions > 0 {
            body["dimensions"] = serde_json::json!(self.requested_dimensions);
        }

        let policy = self.retry_policy;
        with_retry(&policy, "embed", || async {
            let req = self.http.post(&url).json(&body);
            let req = inject_trace_context(req);
            let resp = req.send().await.map_err(classify_reqwest_err)?;
            let status = resp.status();
            if !status.is_success() {
                return Err(classify_status(status, "embed"));
            }
            let body: Value = resp
                .json()
                .await
                .map_err(|e| RetryableError::Permanent(anyhow!("failed to parse: {e}")))?;
            let data = body["data"].as_array().ok_or_else(|| {
                RetryableError::Permanent(anyhow!("missing data[] in response"))
            })?;
            let mut out: Vec<Vec<f32>> = Vec::with_capacity(data.len());
            for item in data {
                let vec: Vec<f32> = item["embedding"]
                    .as_array()
                    .ok_or_else(|| {
                        RetryableError::Permanent(anyhow!("missing embedding in data[]"))
                    })?
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();
                if vec.is_empty() {
                    return Err(RetryableError::Permanent(anyhow!(
                        "empty embedding vector"
                    )));
                }
                out.push(vec);
            }
            Ok(out)
        })
        .await
    }
}

fn classify_reqwest_err(e: reqwest::Error) -> RetryableError {
    // F075: also treat request/body/incomplete-message errors as Transient. A
    // toolgate restart / OOM mid-request surfaces as a dropped-connection body
    // error that is neither is_timeout() nor is_connect(); classifying it
    // Permanent failed the embed with no retry, even though one short backoff
    // would succeed once toolgate is back. Only genuine non-network errors
    // (e.g. builder/decode) stay Permanent.
    if e.is_timeout() || e.is_connect() || e.is_request() || e.is_body() {
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
