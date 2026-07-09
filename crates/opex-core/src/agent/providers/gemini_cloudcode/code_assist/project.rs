//! GCP project context resolution for Code Assist API calls.

#![allow(dead_code)]

use super::types::{CodeAssistError, CODE_ASSIST_ENDPOINT, FREE_TIER_ID, LEGACY_TIER_ID, ProjectContext, code_assist_client};

// ── is_free_tier_quota_error ──────────────────────────────────────────────────

/// Returns `true` when the HTTP response looks like a free-tier per-day quota exhaustion.
///
/// Detection pattern (from Hermes `is_free_tier_quota_error`):
/// - HTTP status is 429
/// - body contains both `"Quota exceeded"` AND `"per-user-per-day"`
///
/// Named as a standalone helper so call sites stay clean and the detection
/// logic has its own focused tests.
pub(super) fn is_free_tier_quota_error(status: u16, body: &str) -> bool {
    status == 429 && body.contains("Quota exceeded") && body.contains("per-user-per-day")
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Resolve (and optionally cache externally) the `ProjectContext` for Code Assist calls.
///
/// - If `stored_project_id` is set (a bare project ID string, already extracted from
///   `RefreshParts::unpack` by the caller in Module 3), returns immediately without any
///   HTTP call. The caller is responsible for calling
///   `crate::agent::providers::gemini_cloudcode::oauth::types::RefreshParts::unpack`
///   on the stored refresh token and passing only `parts.project_id` here.
/// - Otherwise: calls `loadCodeAssist`, then `onboardUser` + LRO poll for free tier.
///
/// Caching (`tokio::sync::Mutex<Option<ProjectContext>>`) lives in `GeminiCloudCodeProvider`
/// (Module 3) — this function is stateless and side-effect-free for testability.
pub async fn ensure_project_ctx(
    #[cfg_attr(test, allow(unused_variables))]
    access_token: &str,
    #[cfg_attr(test, allow(unused_variables))]
    stored_project_id: Option<&str>,
) -> Result<ProjectContext, CodeAssistError> {
    #[cfg(test)]
    {
        // In test builds, return a synthetic free-tier context so provider
        // integration tests don't need a real GCP project or LRO poll.
        // Per D1: function name is ensure_project_ctx.
        // Per D3: all ProjectContext fields are String; empty = "not set".
        Ok(ProjectContext {
            project_id: "test-project".to_string(),
            managed_project_id: "managed-test".to_string(),
            tier_id: FREE_TIER_ID.to_string(),
        })
    }
    #[cfg(not(test))]
    ensure_project_ctx_with_base(access_token, stored_project_id, CODE_ASSIST_ENDPOINT).await
}

/// Test-seam: identical to `ensure_project_ctx` but accepts an explicit `base_url`
/// so integration tests can point at a wiremock server.
pub(super) async fn ensure_project_ctx_with_base(
    access_token: &str,
    stored_project_id: Option<&str>,
    base_url: &str,
) -> Result<ProjectContext, CodeAssistError> {
    // Fast path: project_id already resolved by caller via RefreshParts::unpack.
    // Module 3's resolve_and_cache_project_ctx calls
    //   crate::agent::providers::gemini_cloudcode::oauth::types::RefreshParts::unpack(&c.refresh)
    // and passes parts.project_id here — so this is always a bare project_id string.
    if let Some(project_id) = stored_project_id
        && !project_id.is_empty()
    {
        return Ok(ProjectContext {
            project_id: project_id.to_string(),
            managed_project_id: project_id.to_string(),
            tier_id: FREE_TIER_ID.to_string(),
        });
    }

    let client = code_assist_client()?;

    // Step 1: loadCodeAssist
    let load_url = format!("{}/v1internal:loadCodeAssist", base_url.trim_end_matches('/'));
    let load_resp = client
        .post(&load_url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| CodeAssistError::Http { status: 0, body: e.to_string() })?;

    let load_status = load_resp.status().as_u16();
    let load_body = load_resp.text().await.unwrap_or_default();

    if is_free_tier_quota_error(load_status, &load_body) {
        return Err(CodeAssistError::FreeTierQuotaExhausted { reset_at: None });
    }

    if !(200..300).contains(&load_status) {
        return Err(CodeAssistError::Http { status: load_status, body: load_body });
    }

    let load_json: serde_json::Value = serde_json::from_str(&load_body)
        .map_err(|e| CodeAssistError::Serialization(e.to_string()))?;

    let tier_id = load_json
        .get("tierId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let cloud_project_id = load_json
        .get("cloudProjectId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Paid tier requires an explicit project ID
    let is_free = tier_id == FREE_TIER_ID || tier_id == LEGACY_TIER_ID;
    if !is_free && cloud_project_id.is_none() {
        return Err(CodeAssistError::ProjectIdRequired);
    }

    // Free tier without project: onboard via LRO
    let resolved_project_id = if let Some(proj) = cloud_project_id {
        proj
    } else {
        onboard_and_poll(&client, access_token, base_url, &tier_id).await?
    };

    tracing::info!(
        tier = %tier_id,
        project = %resolved_project_id,
        "resolved Code Assist project context"
    );

    Ok(ProjectContext {
        project_id: resolved_project_id.clone(),
        managed_project_id: resolved_project_id,
        tier_id,
    })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Call `onboardUser` and poll the LRO (max 12 × 5 s = 60 s).
async fn onboard_and_poll(
    client: &reqwest::Client,
    access_token: &str,
    base_url: &str,
    tier_id: &str,
) -> Result<String, CodeAssistError> {
    let onboard_url = format!("{}/v1internal:onboardUser", base_url.trim_end_matches('/'));
    let ob_resp = client
        .post(&onboard_url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({ "tierId": tier_id }))
        .send()
        .await
        .map_err(|e| CodeAssistError::Http { status: 0, body: e.to_string() })?;

    let ob_status = ob_resp.status().as_u16();
    let ob_body = ob_resp.text().await.unwrap_or_default();

    if !(200..300).contains(&ob_status) {
        return Err(CodeAssistError::Http { status: ob_status, body: ob_body });
    }

    let ob_json: serde_json::Value = serde_json::from_str(&ob_body)
        .map_err(|e| CodeAssistError::Serialization(e.to_string()))?;

    let lro_name = ob_json
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            CodeAssistError::Serialization("onboardUser response missing 'name'".to_string())
        })?
        .to_string();

    // Poll LRO
    const MAX_POLLS: u32 = 12;
    const POLL_INTERVAL_MS: u64 = 5000;

    for attempt in 0..MAX_POLLS {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
        }

        let poll_url = format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            lro_name.trim_start_matches('/')
        );
        let poll_resp = client
            .get(&poll_url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| CodeAssistError::Http { status: 0, body: e.to_string() })?;

        let poll_body = poll_resp.text().await.unwrap_or_default();
        let poll_json: serde_json::Value = serde_json::from_str(&poll_body)
            .map_err(|e| CodeAssistError::Serialization(e.to_string()))?;

        if poll_json.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
            if let Some(proj) = poll_json
                .get("response")
                .and_then(|r| r.get("cloudProjectId"))
                .and_then(|v| v.as_str())
            {
                return Ok(proj.to_string());
            }
            return Err(CodeAssistError::Serialization(
                "LRO done but response.cloudProjectId missing".to_string(),
            ));
        }

        tracing::debug!(attempt, lro = %lro_name, "waiting for Code Assist onboarding LRO");
    }

    Err(CodeAssistError::LroTimeout)
}

// NOTE: No local parse_stored_project helper — E15 (Round 3 controller decision):
// The caller (Module 3 resolve_and_cache_project_ctx) is responsible for calling
//   crate::agent::providers::gemini_cloudcode::oauth::types::RefreshParts::unpack(&c.refresh)
// and extracting parts.project_id before passing it into ensure_project_ctx.
// This module receives a bare project_id string only; no packed-string parsing here.

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_free_tier_quota_error unit tests ───────────────────────────────────

    #[test]
    fn free_tier_quota_429_typed() {
        // Exact Hermes-observed pattern
        assert!(is_free_tier_quota_error(
            429,
            r#"{"error":{"message":"Quota exceeded for quota metric 'generate_requests_per_day_per_user' and limit 'GenerateRequestsPerDayPerUser' of service... per-user-per-day"}}"#
        ));
    }

    #[test]
    fn non_429_is_not_quota_error() {
        assert!(!is_free_tier_quota_error(
            500,
            r#"{"error":{"message":"Quota exceeded per-user-per-day"}}"#
        ));
    }

    #[test]
    fn quota_body_without_per_user_per_day_is_not_quota_error() {
        assert!(!is_free_tier_quota_error(
            429,
            r#"{"error":{"message":"Quota exceeded for global limit"}}"#
        ));
    }

    #[test]
    fn empty_body_with_429_is_not_quota_error() {
        assert!(!is_free_tier_quota_error(429, ""));
    }

    #[test]
    fn generic_429_rate_limit_is_not_free_tier_quota_error() {
        assert!(!is_free_tier_quota_error(
            429,
            r#"{"error":{"message":"Too Many Requests"}}"#
        ));
    }

    // ── ensure_project_ctx tests (require wiremock) ───────────────────────────

    #[tokio::test]
    async fn load_code_assist_returns_tier() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tierId": "free-tier",
                "cloudProjectId": "proj-auto-123"
            })))
            .mount(&server)
            .await;

        let ctx = ensure_project_ctx_with_base("fake-token", None, &server.uri())
            .await
            .expect("should succeed");

        assert_eq!(ctx.tier_id, "free-tier");
        assert_eq!(ctx.project_id, "proj-auto-123");
    }

    #[tokio::test]
    async fn paid_tier_without_project_errors() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tierId": "paid-tier"
                // no cloudProjectId
            })))
            .mount(&server)
            .await;

        let err = ensure_project_ctx_with_base("fake-token", None, &server.uri())
            .await
            .expect_err("should fail with ProjectIdRequired");

        assert!(
            matches!(err, CodeAssistError::ProjectIdRequired),
            "expected ProjectIdRequired, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn onboard_polls_lro_until_done() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // loadCodeAssist: free tier, no project yet
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tierId": "free-tier"
                // no cloudProjectId — triggers onboarding
            })))
            .mount(&server)
            .await;

        // onboardUser: returns LRO name
        Mock::given(method("POST"))
            .and(path("/v1internal:onboardUser"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "operations/onboard-op-1"
            })))
            .mount(&server)
            .await;

        // LRO poll: pending once, then done
        Mock::given(method("GET"))
            .and(path("/operations/onboard-op-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "done": false
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/operations/onboard-op-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "done": true,
                "response": {
                    "cloudProjectId": "onboarded-project-456"
                }
            })))
            .mount(&server)
            .await;

        let ctx = ensure_project_ctx_with_base("fake-token", None, &server.uri())
            .await
            .expect("should succeed after LRO");

        assert_eq!(ctx.project_id, "onboarded-project-456");
        assert_eq!(ctx.tier_id, "free-tier");
    }

    #[tokio::test]
    async fn free_tier_quota_429_typed_http() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:loadCodeAssist"))
            .respond_with(
                ResponseTemplate::new(429).set_body_string(
                    r#"{"error":{"message":"Quota exceeded for quota metric per-user-per-day"}}"#,
                ),
            )
            .mount(&server)
            .await;

        let err = ensure_project_ctx_with_base("fake-token", None, &server.uri())
            .await
            .expect_err("should fail with quota error");

        assert!(
            matches!(err, CodeAssistError::FreeTierQuotaExhausted { .. }),
            "expected FreeTierQuotaExhausted, got: {err:?}"
        );
    }
}
