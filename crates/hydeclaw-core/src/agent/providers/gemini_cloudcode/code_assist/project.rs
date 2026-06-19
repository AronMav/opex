//! GCP project context resolution for Code Assist API calls.
//!
//! Manages the lazy-init `ProjectContext` (free-tier auto-assign via LRO,
//! paid-tier explicit project ID). Cached in `GeminiCloudCodeProvider`
//! behind a `tokio::sync::Mutex<Option<ProjectContext>>`.

#![allow(dead_code)]

use super::types::{CodeAssistError, ProjectContext};
use anyhow::Result;

// ── Free-tier quota detection ─────────────────────────────────────────────────

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

// ── ensure_project_ctx — added in Task 6 ─────────────────────────────────────

/// Resolve (and cache) the `ProjectContext` for Code Assist API calls.
///
/// Implementation added in Task 6. Placeholder signature declared here so
/// `code_assist/mod.rs` can re-export it without a compile gap between tasks.
pub async fn ensure_project_ctx(
    _access_token: &str,
    _stored_project_id: Option<&str>,
) -> Result<ProjectContext, CodeAssistError> {
    // Task 6 will replace this body with full LRO-based resolution.
    // This stub satisfies the cross-module interface contract (signature matches spec).
    Err(CodeAssistError::ProjectIdRequired)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
