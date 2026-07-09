//! Core types for the Code Assist API client.

// Types are fully wired up in Modules 3–4; suppress dead_code until then.
#![allow(dead_code)]

use thiserror::Error;

// ── Endpoints ────────────────────────────────────────────────────────────────

pub const CODE_ASSIST_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";

// ── Tier IDs (from Hermes google_code_assist.py) ─────────────────────────────

pub const FREE_TIER_ID: &str = "free-tier";
pub const LEGACY_TIER_ID: &str = "legacy-free-tier";

// ── ProjectContext ────────────────────────────────────────────────────────────

/// Resolved GCP project context for Code Assist API calls.
/// Cached after first resolution; persisted back into the OAuth token's
/// `refresh` field as packed strings (Hermes-compatible format).
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// The GCP project ID (empty string for free-tier managed projects).
    pub project_id: String,
    /// The managed project ID assigned by Google's free-tier onboarding LRO.
    pub managed_project_id: String,
    /// Tier identifier, e.g. "free-tier" or "legacy-free-tier".
    pub tier_id: String,
}

// ── CodeAssistError ───────────────────────────────────────────────────────────

/// Typed errors returned by the Code Assist API client.
#[derive(Debug, Error)]
pub enum CodeAssistError {
    /// Paid tier detected but no GCP project ID was provided.
    /// Caller must surface a UI prompt asking the user for their project ID.
    #[error("GCP project ID required for paid-tier Code Assist")]
    ProjectIdRequired,

    /// Free-tier is unavailable in the user's region or for their account.
    #[error("Code Assist free tier unavailable: {reason}")]
    FreeTierUnavailable { reason: String },

    /// Free-tier per-user-per-day quota exhausted.
    /// `reset_at` is the UTC timestamp when the quota resets, if parseable
    /// from the response.
    #[error("Code Assist free-tier quota exhausted (resets at {reset_at:?})")]
    FreeTierQuotaExhausted { reset_at: Option<chrono::DateTime<chrono::Utc>> },

    /// LRO onboarding polling timed out (12 × 5s = 60s).
    #[error("Code Assist onboarding LRO timed out after 60 seconds")]
    LroTimeout,

    /// Non-2xx HTTP response.
    #[error("Code Assist HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// JSON (de)serialisation failure.
    #[error("Code Assist serialisation error: {0}")]
    Serialization(String),
}

// ── HTTP client ────────────────────────────────────────────────────────────────

/// Build the reqwest client used for all Code Assist project/quota HTTP calls.
///
/// F007: previously these call sites used `reqwest::Client::new()`, which has
/// NO connect/request timeout, so a stalled `cloudcode-pa.googleapis.com`
/// (accepts the TCP connection but never responds) would hang the request/cron
/// task indefinitely. This mirrors the timeouts already set on the OAuth
/// refresh/device-flow clients (`.use_rustls_tls().timeout(20s)`), with an
/// added `connect_timeout` so a black-holed endpoint fails fast enough to
/// surface an error / fail over.
pub(super) fn code_assist_client() -> Result<reqwest::Client, CodeAssistError> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| CodeAssistError::Http { status: 0, body: e.to_string() })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_tier_id_constant_value() {
        assert_eq!(FREE_TIER_ID, "free-tier");
    }

    #[test]
    fn legacy_tier_id_constant_value() {
        assert_eq!(LEGACY_TIER_ID, "legacy-free-tier");
    }

    #[test]
    fn code_assist_endpoint_constant_value() {
        assert_eq!(CODE_ASSIST_ENDPOINT, "https://cloudcode-pa.googleapis.com");
    }

    #[test]
    fn project_context_fields_accessible() {
        let ctx = ProjectContext {
            project_id: "proj-123".to_string(),
            managed_project_id: "managed-456".to_string(),
            tier_id: FREE_TIER_ID.to_string(),
        };
        assert_eq!(ctx.project_id, "proj-123");
        assert_eq!(ctx.managed_project_id, "managed-456");
        assert_eq!(ctx.tier_id, "free-tier");
    }

    #[test]
    fn code_assist_error_project_id_required_display() {
        let e = CodeAssistError::ProjectIdRequired;
        let s = e.to_string();
        assert!(s.contains("project") || s.contains("Project"), "display: {s}");
    }

    #[test]
    fn code_assist_error_free_tier_quota_exhausted_no_reset() {
        let e = CodeAssistError::FreeTierQuotaExhausted { reset_at: None };
        let _ = e.to_string(); // must not panic
    }

    #[test]
    fn code_assist_error_http_carries_status_and_body() {
        let e = CodeAssistError::Http { status: 429, body: "Quota exceeded".to_string() };
        let s = e.to_string();
        assert!(s.contains("429"), "display: {s}");
    }
}
