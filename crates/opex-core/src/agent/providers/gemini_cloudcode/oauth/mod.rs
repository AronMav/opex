//! # Acceptance criteria (D20)
//!
//! CI passes when:
//! 1. `make test` compiles and runs `gemini_cloudcode::` tests (feature flag included).
//! 2. `make llvm-cov` shows ≥80% line coverage for each file in this subtree.
//! 3. The no-token-log grep returns zero matches:
//!    ```sh
//!    rg -n 'access_token|refresh_token' \
//!        crates/opex-core/src/agent/providers/gemini_cloudcode/ \
//!      | rg -v '(redact|#\[cfg\(test\)\]|pub |struct |fn |let |field|json\[|\.get\(|"access_token"|"refresh_token")'
//!    ```

// Module 1 Task 1: OAuth foundation types.
// Constants are used by later tasks (Modules 2–4); suppress dead_code until wired up.
#![allow(dead_code)]

pub mod client_creds;
pub mod device;
pub mod device_flow;
pub mod flow;
pub mod pkce;
pub mod refresh;
pub mod storage;
pub mod types;

// Re-export core types so callers can use `oauth::GoogleCredentials` directly.
#[allow(unused_imports)]
pub use types::{DeviceFlowSession, GoogleCredentials, OauthError, RefreshParts};

// OAuth endpoint constants.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
pub const DEVICE_CODE_ENDPOINT: &str = "https://oauth2.googleapis.com/device/code";
pub const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
pub const OAUTH_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
    https://www.googleapis.com/auth/userinfo.email \
    https://www.googleapis.com/auth/userinfo.profile";
pub const REDIRECT_HOST: &str = "127.0.0.1";
pub const DEFAULT_REDIRECT_PORT: u16 = 8085;
pub const CALLBACK_PATH: &str = "/oauth2callback";
pub const REFRESH_SKEW_SECONDS: i64 = 60;

// ── CI security check (run manually or in CI) ──────────────────────────────
// Verify no bare token values leak into tracing calls:
//   rg -n 'access_token|refresh_token' \
//       crates/opex-core/src/agent/providers/gemini_cloudcode/ \
//     | rg -v '(redact|#\[cfg\(test\)\]|pub |struct |fn |let |field|json\[|\.get\()'
// Must return zero matches (exit code 1 from second rg = no matches = pass).

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// Verify that the public API surface declared in the cross-module contract
    /// compiles and is accessible.
    #[test]
    fn public_api_surface_accessible() {
        // GoogleCredentials struct fields
        let _c: GoogleCredentials = GoogleCredentials {
            refresh: "r".to_string(),
            access: "a".to_string(),
            expires_ms: 0,
            email: "e".to_string(),
        };
        // OauthError variants
        let _e1 = OauthError::NotAuthenticated;
        let _e2 = OauthError::ReAuthRequired;
        let _e3 = OauthError::StateMismatch;
        let _e4 = OauthError::Timeout;
        let _e5 = OauthError::TokenExchange { status: 400, body: "b".to_string() };
        let _e6 = OauthError::PortBindFailed;
        let _e7 = OauthError::DeviceExpired;
        let _e8 = OauthError::DeviceAccessDenied;
        let _e9 = OauthError::LockTimeout;
        // DeviceFlowSession
        let _s = DeviceFlowSession {
            device_code: "d".to_string(),
            user_code: "u".to_string(),
            verification_uri: "v".to_string(),
            interval_secs: 5,
            expires_in_secs: 1800,
        };
        // Constants
        let _: &str = AUTH_ENDPOINT;
        let _: &str = TOKEN_ENDPOINT;
        let _: &str = DEVICE_CODE_ENDPOINT;
        let _: &str = USERINFO_ENDPOINT;
        let _: &str = OAUTH_SCOPES;
        let _: u16 = DEFAULT_REDIRECT_PORT;
        let _: i64 = REFRESH_SKEW_SECONDS;
    }
}
