//! Google Gemini Code Assist OAuth provider.
//!
//! Enabled by the `gemini-cloudcode` Cargo feature. All public items are
//! re-exported through this module; downstream code imports from
//! `crate::agent::providers::gemini_cloudcode::*`.
//!
//! Module 1 creates this file with `mod oauth;` only.
//! Modules 2–4 amend this file to add their own submodule declarations.

pub mod oauth;
pub mod code_assist;
pub mod stream;
pub mod provider;
pub(crate) use provider::GeminiCloudCodeProvider;
// NOTE: Module 4 appends its mod declaration here:
//   pub mod ui_api;   ← Module 4
