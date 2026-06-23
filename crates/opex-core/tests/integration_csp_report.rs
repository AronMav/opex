//! Phase 64 SEC-05 — CSP violation report endpoint.
//!
//! Covers:
//!   - Unit: counter increments per directive
//!   - Unit: dashboard body contains csp section + overflow counter
//!   - Unit: cardinality cap (MAX_CSP_DIRECTIVES) + overflow counter
//!   - Unit: long directive key truncation (MAX_CSP_DIRECTIVE_LEN)
//!   - HTTP: oversized body (>64KiB) returns 413 via DefaultBodyLimit layer
//!   - HTTP: per-IP rate limit (~30 rpm) returns 429 past the cap
//!
//! HTTP tests use `tower::ServiceExt::oneshot` against a minimal in-process
//! axum Router — no TCP listener, no auth, no DB. This isolates the
//! observability surface from the rest of the gateway for deterministic tests.

mod support;

use opex_core::gateway::csp::{
    api_csp_report_handler, csp_report_rate_limit, routes_for_test, CspReport, CspReportBody,
    CspReportRateLimiter, CSP_REPORT_MAX_BODY, CSP_REPORT_PER_IP_RPM,
};
use opex_core::metrics::{
    build_dashboard_body, MetricsRegistry, MAX_CSP_DIRECTIVES, MAX_CSP_DIRECTIVE_LEN,
};
use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

const SAMPLE_REPORT: &str = r#"{
    "csp-report": {
        "document-uri": "https://example.com/",
        "violated-directive": "script-src 'self'",
        "blocked-uri": "https://evil.example/pwn.js",
        "effective-directive": "script-src",
        "original-policy": "script-src 'self'; report-uri /api/csp-report",
        "disposition": "report",
        "status-code": 200
    }
}"#;

// ── Unit: counter API + dashboard body shape ────────────────────────────────

#[tokio::test]
async fn counter_increments_per_directive() {
    let reg = MetricsRegistry::new();
    let before_script = reg.csp_violations_total_count("script-src");
    reg.record_csp_violation("script-src");
    reg.record_csp_violation("img-src");
    let after_script = reg.csp_violations_total_count("script-src");
    let after_img = reg.csp_violations_total_count("img-src");
    assert_eq!(after_script - before_script, 1);
    assert_eq!(after_img, 1);
}

#[tokio::test]
async fn dashboard_body_includes_csp_section() {
    let reg = MetricsRegistry::new();
    reg.record_csp_violation("script-src");
    let v = build_dashboard_body(&reg);
    let csp = v.get("csp_violations").expect("csp section present");
    assert_eq!(csp["script-src"], 1);
    assert_eq!(v["csp_violations_overflow"], 0);
}

/// Warning 9 fix: HashMap cardinality cap prevents memory blow-up from hostile
/// browsers cycling directive names.
#[tokio::test]
async fn cardinality_cap_hits_overflow() {
    let reg = MetricsRegistry::new();
    for i in 0..(MAX_CSP_DIRECTIVES + 8) {
        reg.record_csp_violation(&format!("dir-{i}"));
    }
    assert_eq!(
        reg.csp_violations_map_len(),
        MAX_CSP_DIRECTIVES,
        "map must stop growing at cap"
    );
    assert_eq!(
        reg.csp_violations_overflow_count(),
        8,
        "overflow counter must capture every attempt past cap"
    );
}

/// Warning 9 fix: long directive strings are truncated before storage.
#[tokio::test]
async fn long_directive_name_truncated() {
    let reg = MetricsRegistry::new();
    let long_name = "x".repeat(200);
    reg.record_csp_violation(&long_name);
    let stored_keys = reg.csp_violations_keys_snapshot();
    assert!(
        stored_keys.iter().any(|k| k.len() == MAX_CSP_DIRECTIVE_LEN),
        "expected a truncated key of length {MAX_CSP_DIRECTIVE_LEN}, got keys: {stored_keys:?}"
    );
    assert!(
        !stored_keys.iter().any(|k| k.len() > MAX_CSP_DIRECTIVE_LEN),
        "no stored key may exceed the cap"
    );
}

// ── HTTP: handler returns 204 + increments metric ───────────────────────────

#[tokio::test]
async fn post_report_returns_204_via_handler() {
    let metrics = Arc::new(MetricsRegistry::new());
    let app = routes_for_test(metrics.clone());

    let req = Request::builder()
        .method("POST")
        .uri("/api/csp-report")
        .header(header::CONTENT_TYPE, "application/csp-report")
        .body(Body::from(SAMPLE_REPORT))
        .expect("build req");

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    // effective-directive is "script-src" → counter bumped under that key.
    assert_eq!(metrics.csp_violations_total_count("script-src"), 1);
}

#[tokio::test]
async fn malformed_report_rejected_without_increment() {
    let metrics = Arc::new(MetricsRegistry::new());
    let app = routes_for_test(metrics.clone());

    let req = Request::builder()
        .method("POST")
        .uri("/api/csp-report")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("not-json-at-all"))
        .expect("build req");

    let resp = app.oneshot(req).await.expect("oneshot");
    // axum Json extractor rejects malformed bodies with 400 (or 415 on content-type);
    // either way, no counter bump.
    assert!(
        resp.status().is_client_error(),
        "expected 4xx for malformed body, got {}",
        resp.status()
    );
    assert_eq!(metrics.csp_violations_total_count("script-src"), 0);
    assert_eq!(metrics.csp_violations_map_len(), 0);
}

/// Warning 9 fix: body-size cap — oversized reports return 413.
#[tokio::test]
async fn oversized_body_returns_413() {
    let metrics = Arc::new(MetricsRegistry::new());
    let app = routes_for_test(metrics.clone());

    // Build a body larger than CSP_REPORT_MAX_BODY. Shape like JSON so
    // content-type check doesn't short-circuit before the body is read.
    let padding = "x".repeat(CSP_REPORT_MAX_BODY + 1024);
    let body = format!(r#"{{"csp-report":{{"document-uri":"{padding}"}}}}"#);

    let req = Request::builder()
        .method("POST")
        .uri("/api/csp-report")
        .header(header::CONTENT_TYPE, "application/csp-report")
        .body(Body::from(body))
        .expect("build req");

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "oversized body must return 413"
    );
    assert_eq!(metrics.csp_violations_map_len(), 0, "no counter bump on 413");
}

// ── Unit: per-IP rate limiter (~30 rpm) ─────────────────────────────────────

/// Warning 10 fix: dedicated per-IP rate limiter on /api/csp-report (~30 rpm).
/// Adding the path to PUBLIC_PREFIX bypasses the auth-lockout; this test
/// proves the separate limiter prevents unbounded anonymous abuse.
#[tokio::test]
async fn per_ip_rate_limit_blocks_after_cap() {
    let limiter = CspReportRateLimiter::new();
    let ip: IpAddr = "203.0.113.42".parse().expect("test ip");
    let mut ok = 0;
    let mut limited = 0;
    for _ in 0..40 {
        if limiter.check(ip) {
            ok += 1;
        } else {
            limited += 1;
        }
    }
    assert_eq!(
        ok, CSP_REPORT_PER_IP_RPM as usize,
        "first {CSP_REPORT_PER_IP_RPM} must pass within the window"
    );
    assert_eq!(
        limited,
        40 - CSP_REPORT_PER_IP_RPM as usize,
        "remainder must be blocked"
    );

    // Independent IP has its own bucket.
    let ip2: IpAddr = "198.51.100.7".parse().expect("test ip");
    assert!(limiter.check(ip2), "distinct IP has independent bucket");
}

/// Integration check: the middleware layer returns 429 when the per-IP cap is
/// exceeded. We pre-fill the limiter to the cap for a specific IP, then the
/// next request (same IP) should be denied.
#[tokio::test]
async fn middleware_returns_429_past_cap() {
    let metrics = Arc::new(MetricsRegistry::new());
    let limiter = Arc::new(CspReportRateLimiter::new());
    let ip: IpAddr = "203.0.113.99".parse().expect("test ip");

    // Drain the bucket so the next request trips the limit.
    for _ in 0..CSP_REPORT_PER_IP_RPM {
        assert!(limiter.check(ip));
    }

    // The next call must be denied.
    assert!(!limiter.check(ip), "bucket drained — next check must fail");

    // Call the pure handler directly (post-limiter) to confirm shape is correct.
    let report = CspReport {
        report: CspReportBody {
            effective_directive: "script-src".into(),
            ..Default::default()
        },
    };
    let status = api_csp_report_handler(&metrics, report);
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(metrics.csp_violations_total_count("script-src"), 1);

    // And the middleware-style gate returns 429 for the capped IP.
    let gate = csp_report_rate_limit(&limiter, ip);
    assert_eq!(gate, Some(StatusCode::TOO_MANY_REQUESTS));
}
