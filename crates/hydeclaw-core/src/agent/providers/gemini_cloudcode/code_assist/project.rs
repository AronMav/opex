//! GCP project context resolution for the Code Assist API.
//!
//! Implements `load_code_assist`, `onboard_user`, and `ensure_project_ctx`
//! which together resolve the `ProjectContext` required for every Code Assist
//! API call.
//!
//! Full implementation arrives in Module 2 Task 4 (project + quota phase).

// Stub — full implementation arrives in M2 T4.
#![allow(dead_code)]

use anyhow::Result;

use crate::agent::providers::gemini_cloudcode::code_assist::types::ProjectContext;

/// Resolve a `ProjectContext` for the given access token, loading it from
/// the Code Assist API or triggering free-tier onboarding if necessary.
///
/// Placeholder — always returns an error until M2 T4.
pub async fn ensure_project_ctx(
    _access_token: &str,
    _project_id_hint: Option<&str>,
    _http_client: &reqwest::Client,
) -> Result<ProjectContext> {
    anyhow::bail!("ensure_project_ctx: not yet implemented (M2 T4)")
}
