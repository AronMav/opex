//! Informational quota query for Code Assist free tier.
//!
//! Calls `POST /v1internal:retrieveUserQuota`. Not in the hot path —
//! used only by operator tooling (e.g., a future /gquota UI endpoint).

use super::types::{CodeAssistError, CODE_ASSIST_ENDPOINT, code_assist_client};
use chrono::{DateTime, Utc};

// ── Types ─────────────────────────────────────────────────────────────────────

/// A single quota bucket returned by `retrieveUserQuota`.
#[derive(Debug, Clone)]
pub struct QuotaBucket {
    /// Quota metric name (e.g. `"generate_requests_per_day_per_user"`).
    pub metric: String,
    /// Hard limit for this bucket.
    pub limit: i64,
    /// Current usage within the reset window.
    pub used: i64,
    /// UTC timestamp when this bucket resets, if available from the API.
    pub resets_at: Option<DateTime<Utc>>,
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Query the Code Assist quota endpoint for the given project.
///
/// Returns an empty vec (not an error) when the API returns no `quotas` array.
/// Non-2xx responses surface as `CodeAssistError::Http`.
pub async fn retrieve_user_quota(
    access_token: &str,
    project_id: &str,
) -> Result<Vec<QuotaBucket>, CodeAssistError> {
    retrieve_user_quota_with_base(access_token, project_id, CODE_ASSIST_ENDPOINT).await
}

/// Test-seam: identical to `retrieve_user_quota` but accepts an explicit `base_url`.
pub(super) async fn retrieve_user_quota_with_base(
    access_token: &str,
    project_id: &str,
    base_url: &str,
) -> Result<Vec<QuotaBucket>, CodeAssistError> {
    let client = code_assist_client()?;
    let url = format!(
        "{}/v1internal:retrieveUserQuota",
        base_url.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "cloudProjectId": project_id }))
        .send()
        .await
        .map_err(|e| CodeAssistError::Http { status: 0, body: e.to_string() })?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();

    if !(200..300).contains(&status) {
        tracing::warn!(status, "retrieve_user_quota returned non-2xx");
        return Err(CodeAssistError::Http { status, body });
    }

    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| CodeAssistError::Serialization(e.to_string()))?;

    let buckets = json
        .get("quotas")
        .and_then(|q| q.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|q| QuotaBucket {
            metric: q
                .get("metric")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            limit: q
                .get("limit")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            used: q
                .get("usage")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            resets_at: None,
        })
        .collect();

    Ok(buckets)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retrieve_user_quota_happy_path() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:retrieveUserQuota"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "quotas": [
                    {
                        "metric": "generate_requests_per_day_per_user",
                        "limit": 1000,
                        "usage": 42
                    }
                ]
            })))
            .mount(&server)
            .await;

        let buckets = retrieve_user_quota_with_base(
            "fake-token",
            "my-project",
            &server.uri(),
        )
        .await
        .expect("should succeed");

        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].metric, "generate_requests_per_day_per_user");
        assert_eq!(buckets[0].limit, 1000);
        assert_eq!(buckets[0].used, 42);
    }
}
