//! Bearer exemption for the global request rate limiter.
//!
//! The per-IP `RequestRateLimiter` budget exists to shield the server from
//! anonymous abuse. Authenticated callers (valid gateway token) are exempt —
//! the web UI's polling (tasks/sessions/notifications across several open
//! tabs) legitimately exceeds a small per-IP budget, and throttling it
//! surfaces as 429 storms in the browser. These tests pin the header
//! validation used by `request_rate_limit_middleware` to decide exemption:
//! only an exact constant-time match of `Authorization: Bearer <token>`
//! qualifies; absent, malformed, or wrong tokens still consume the budget.

use axum::http::HeaderMap;
use opex_core::gateway::middleware::valid_bearer;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn headers_with_auth(value: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("authorization", value.parse().expect("valid header value"));
    h
}

#[test]
fn exact_bearer_token_is_exempt() {
    let headers = headers_with_auth(&format!("Bearer {TOKEN}"));
    assert!(valid_bearer(&headers, TOKEN));
}

#[test]
fn missing_authorization_header_is_not_exempt() {
    let headers = HeaderMap::new();
    assert!(!valid_bearer(&headers, TOKEN));
}

#[test]
fn wrong_token_is_not_exempt() {
    let headers = headers_with_auth("Bearer wrong-token-entirely");
    assert!(!valid_bearer(&headers, TOKEN));
}

#[test]
fn token_with_trailing_garbage_is_not_exempt() {
    let headers = headers_with_auth(&format!("Bearer {TOKEN}x"));
    assert!(!valid_bearer(&headers, TOKEN));
}

#[test]
fn truncated_token_is_not_exempt() {
    // One char short of TOKEN (ASCII, so no char-boundary concern — but the
    // repo denies clippy::string_slice, hence the explicit literal).
    const TRUNCATED: &str = "0123456789abcdef0123456789abcde";
    let headers = headers_with_auth(&format!("Bearer {TRUNCATED}"));
    assert!(!valid_bearer(&headers, TOKEN));
}

#[test]
fn non_bearer_scheme_is_not_exempt() {
    let headers = headers_with_auth(&format!("Basic {TOKEN}"));
    assert!(!valid_bearer(&headers, TOKEN));
}

#[test]
fn bearer_without_space_is_not_exempt() {
    let headers = headers_with_auth(&format!("Bearer{TOKEN}"));
    assert!(!valid_bearer(&headers, TOKEN));
}
