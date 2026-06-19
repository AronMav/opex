//! Code Assist API client sub-modules.
//!
//! Items declared here are used by Modules 3–4 once the provider wire-up
//! is complete. Allow dead_code / unused_imports at the module level so
//! clippy stays clean while the stubs are in place.
#![allow(dead_code, unused_imports)]

pub mod types;
pub mod schema;
pub mod request;
pub mod response;
pub mod project;

pub use types::{
    CODE_ASSIST_ENDPOINT, FREE_TIER_ID, LEGACY_TIER_ID,
    CodeAssistError, ProjectContext,
};
pub use schema::sanitize_gemini_tool_parameters;
pub use request::{build_gemini_request, wrap_code_assist_request};
pub use response::translate_gemini_response;
pub use project::ensure_project_ctx;
