//! Phase 64 SEC-01 — unified into [`crate::net::ssrf`].
//!
//! This module is now a **re-export shim** so existing call sites keep
//! compiling unchanged. Prefer `crate::net::ssrf::*` in new code.
//!
//! Migration map:
//!
//! | Old path                                      | New path                                  |
//! | --------------------------------------------- | ----------------------------------------- |
//! | `crate::tools::ssrf::validate_url_scheme`     | `crate::net::ssrf::validate_url_scheme`   |
//! | `crate::tools::ssrf::is_internal_endpoint`    | `crate::net::ssrf::is_internal_endpoint`  |
//! | `crate::tools::ssrf::SsrfSafeResolver`        | `crate::net::ssrf::SsrfSafeResolver`      |
//!
//! `validate_url_scheme` used to return `anyhow::Result<()>` but now returns
//! `Result<(), crate::net::ssrf::SsrfError>` — callers that used `?` still
//! compile because `SsrfError: std::error::Error`.

#[allow(unused_imports)]
pub use crate::net::ssrf::{
    is_internal_endpoint, preflight_resolve, ssrf_http_client, validate_url_scheme, SsrfError,
    SsrfSafeResolver,
};
