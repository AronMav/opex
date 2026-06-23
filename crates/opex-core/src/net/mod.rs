//! Unified outbound-network safety module.
//!
//! Phase 64 SEC-01: all outbound HTTP destined for user-controlled URLs
//! (YAML tools, future webhook outbound deliveries, link preview fetchers,
//! provider discovery probes) MUST go through [`ssrf::ssrf_http_client`] so
//! the SSRF-safe DNS resolver + redirect=none policy are applied uniformly.
//!
//! The leaf module `ssrf` has zero `crate::*` dependencies so re-exposing it
//! through `src/lib.rs` does NOT cascade the integration-test facade past its
//! 10-module budget.

pub mod ssrf;
