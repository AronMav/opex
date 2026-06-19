//! Core types for the Gemini Cloud Code OAuth module.

// Types and constants in this module are fully wired up in later tasks
// (Modules 2–4). Suppress dead_code lint until then.
#![allow(dead_code)]

use thiserror::Error;

// ── RefreshParts (D17) ────────────────────────────────────────────────────

/// Typed representation of the packed `refresh` field in `GoogleCredentials`.
///
/// The `refresh` field stores `<refresh_token>|<project_id>|<managed_project_id>`.
/// This struct provides type-safe pack/unpack so callers never hand-roll string
/// splits. (D17: explicit type required by cross-module contract.)
#[derive(Debug, Clone, Default)]
pub struct RefreshParts {
    pub refresh_token: String,
    pub project_id: String,
    pub managed_project_id: String,
}

impl RefreshParts {
    /// Serialize to the packed string stored in `GoogleCredentials.refresh`.
    pub fn pack(&self) -> String {
        format!("{}|{}|{}", self.refresh_token, self.project_id, self.managed_project_id)
    }

    /// Deserialize from a packed string. Tolerates bare tokens (no `|` separators).
    pub fn unpack(packed: &str) -> Self {
        let mut parts = packed.splitn(3, '|');
        Self {
            refresh_token: parts.next().unwrap_or("").to_string(),
            project_id: parts.next().unwrap_or("").to_string(),
            managed_project_id: parts.next().unwrap_or("").to_string(),
        }
    }
}

// ── GoogleCredentials ──────────────────────────────────────────────────────

/// Persisted OAuth credential, binary-compatible with Hermes Agent's
/// `google_oauth.json` format (structural idea from Hermes; no code copied).
///
/// The `refresh` field is the packed output of `RefreshParts::pack()`:
/// `<refresh_token>|<project_id>|<managed_project_id>`.
/// Empty IDs are valid: `sometoken||` means no project IDs resolved yet.
/// Use `RefreshParts::unpack(&self.refresh)` to access individual fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoogleCredentials {
    /// Packed `refresh_token|project_id|managed_project_id` (see `RefreshParts`).
    pub refresh: String,
    /// Current short-lived access token.
    pub access: String,
    /// Expiry as Unix epoch milliseconds. JSON field name is `"expires"`.
    #[serde(rename = "expires")]
    pub expires_ms: i64,
    /// User email, fetched from USERINFO_ENDPOINT (best-effort; may be empty).
    pub email: String,
}

impl GoogleCredentials {
    /// Convenience: unpack the refresh field into a `RefreshParts`.
    pub fn refresh_parts(&self) -> RefreshParts {
        RefreshParts::unpack(&self.refresh)
    }

    /// Convenience: returns only the raw refresh token (first segment of packed field).
    pub fn refresh_token(&self) -> &str {
        self.refresh.split('|').next().unwrap_or("")
    }
}

// ── DeviceFlowSession ──────────────────────────────────────────────────────

/// In-progress device-code session returned by `login_device_flow`.
/// The caller polls `TOKEN_ENDPOINT` using `device_code` until granted or expired.
#[derive(Debug, Clone)]
pub struct DeviceFlowSession {
    /// Opaque code sent to the token endpoint during polling.
    pub device_code: String,
    /// Human-readable code shown to the user.
    pub user_code: String,
    /// URL the user visits to enter `user_code`.
    pub verification_uri: String,
    /// Minimum seconds between poll attempts.
    pub interval_secs: u64,
    /// Total seconds until the device code expires.
    pub expires_in_secs: u64,
}

// ── OauthError ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum OauthError {
    #[error("not authenticated — run login flow first")]
    NotAuthenticated,
    #[error("re-authentication required (refresh token revoked or expired)")]
    ReAuthRequired,
    #[error("OAuth state mismatch — possible CSRF on local callback port")]
    StateMismatch,
    #[error("timed out waiting for OAuth callback (5-minute window)")]
    Timeout,
    #[error("token exchange failed (HTTP {status}): {body}")]
    TokenExchange { status: u16, body: String },
    #[error("could not bind local callback port in range 8085–8089")]
    PortBindFailed,
    #[error("device code expired — user did not authorize in time")]
    DeviceExpired,
    #[error("device access denied by user")]
    DeviceAccessDenied,
    #[error("timed out acquiring credentials file lock (30 s)")]
    LockTimeout,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_fields_accessible() {
        let cred = GoogleCredentials {
            refresh: "tok|proj|managed".to_string(),
            access: "ya29.x".to_string(),
            expires_ms: 1_750_000_000_000,
            email: "user@example.com".to_string(),
        };
        assert_eq!(cred.email, "user@example.com");
        assert_eq!(cred.expires_ms, 1_750_000_000_000);
    }

    #[test]
    fn oauth_error_display_not_authenticated() {
        let e = OauthError::NotAuthenticated;
        assert!(e.to_string().contains("not authenticated"));
    }

    #[test]
    fn device_flow_session_fields() {
        let s = DeviceFlowSession {
            device_code: "dc".to_string(),
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://google.com/device".to_string(),
            interval_secs: 5,
            expires_in_secs: 1800,
        };
        assert_eq!(s.user_code, "ABCD-EFGH");
    }

    // D17: RefreshParts typed pack/unpack
    #[test]
    fn refresh_parts_pack_unpack_roundtrip() {
        let parts = RefreshParts {
            refresh_token: "mytoken".to_string(),
            project_id: "proj123".to_string(),
            managed_project_id: "managed456".to_string(),
        };
        let packed = parts.pack();
        assert_eq!(packed, "mytoken|proj123|managed456");
        let unpacked = RefreshParts::unpack(&packed);
        assert_eq!(unpacked.refresh_token, "mytoken");
        assert_eq!(unpacked.project_id, "proj123");
        assert_eq!(unpacked.managed_project_id, "managed456");
    }

    #[test]
    fn refresh_parts_unpack_bare_token() {
        let unpacked = RefreshParts::unpack("baretoken");
        assert_eq!(unpacked.refresh_token, "baretoken");
        assert_eq!(unpacked.project_id, "");
        assert_eq!(unpacked.managed_project_id, "");
    }

    #[test]
    fn credentials_refresh_parts_convenience() {
        let cred = GoogleCredentials {
            refresh: "tok|proj|managed".to_string(),
            access: "a".to_string(),
            expires_ms: 0,
            email: String::new(),
        };
        let parts = cred.refresh_parts();
        assert_eq!(parts.refresh_token, "tok");
        assert_eq!(parts.project_id, "proj");
        assert_eq!(parts.managed_project_id, "managed");
    }

    #[test]
    fn credentials_serde_expires_rename() {
        // The JSON field must be "expires" (not "expires_ms") per E4 controller decision.
        let cred = GoogleCredentials {
            refresh: "r|p|m".to_string(),
            access: "a".to_string(),
            expires_ms: 999,
            email: "x@example.com".to_string(),
        };
        let json = serde_json::to_string(&cred).expect("serialize");
        assert!(json.contains("\"expires\":999"), "expected 'expires' key, got: {json}");
        assert!(!json.contains("expires_ms"), "unexpected 'expires_ms' key in: {json}");

        // Round-trip via deserialization.
        let parsed: GoogleCredentials = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.expires_ms, 999);
    }
}
