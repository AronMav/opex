//! Google Gemini Code Assist OAuth provider.
//!
//! Enabled by the `gemini-cloudcode` Cargo feature. All public items are
//! re-exported through this module; downstream code imports from
//! `crate::agent::providers::gemini_cloudcode::*`.
//!
//! Module 1 creates this file with `mod oauth;` only.
//! Modules 2–4 amend this file to add their own submodule declarations.

pub mod oauth;
// NOTE: Modules 2–4 append their mod declarations here:
//   pub mod code_assist;  ← Module 2
//   pub mod stream;       ← Module 3
//   pub mod provider;     ← Module 3
//   pub mod ui_api;       ← Module 4
