//! Phase 65 OBS-04 — W3C Trace Context middleware.
//!
//! Validates the leaf `gateway::trace_context` module exposes:
//!   * `parse_traceparent(&str) -> Option<TraceId>` — strict W3C version-00
//!     parser; fail-open on any format deviation (reserved zero, wrong length,
//!     unsupported version, non-hex characters).
//!   * `new_trace_id() -> TraceId` — 32-char lowercase-hex UUID v4.
//!   * `trace_context_middleware` — Axum middleware that parses or generates
//!     a trace_id, attaches it as a request extension, and records it on the
//!     current tracing span so every `tracing::info!`/`tracing::error!` call
//!     in the request lifecycle carries the trace_id.
//!
//! Middleware behavior (HTTP tests use `tower::ServiceExt::oneshot` against
//! a minimal in-process Router — no TCP listener, no auth, no DB):
//!   * Test 5 `middleware_generates_id_when_header_missing` — absent
//!     `traceparent` → new 32-hex trace_id.
//!   * Test 6 `middleware_preserves_valid_traceparent` — inbound valid
//!     `traceparent` → trace_id is propagated unchanged.
//!   * Test 7 `middleware_fails_open_on_malformed` — malformed `traceparent`
//!     → server generates a fresh trace_id (NOT 400).

use axum::{
    Router,
    body::Body,
    extract::Extension,
    http::{Request, Response, StatusCode},
    middleware as axum_mw,
    response::IntoResponse,
    routing::get,
};
use opex_core::gateway::trace_context::{
    TraceId, new_trace_id, parse_traceparent, trace_context_middleware,
};
use tower::ServiceExt;

// ── Small echo handler that reflects the Extension<TraceId> into a response header ──

async fn echo_handler(Extension(tid): Extension<TraceId>) -> Response<Body> {
    let mut resp: Response<Body> = ().into_response();
    resp.headers_mut()
        .insert("X-Trace-Id", tid.0.parse().expect("valid header value"));
    resp
}

fn build_app() -> Router {
    Router::new()
        .route("/echo", get(echo_handler))
        .layer(axum_mw::from_fn(trace_context_middleware))
}

// ── Unit: parse_traceparent ───────────────────────────────────────────────────

#[test]
fn parse_valid_traceparent_returns_trace_id() {
    let t = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        .expect("valid traceparent parses");
    assert_eq!(t.0, "4bf92f3577b34da6a3ce929d0e0e4736");
}

#[test]
fn parse_missing_traceparent_returns_none() {
    assert!(parse_traceparent("").is_none());
}

#[test]
fn parse_malformed_traceparent_returns_none() {
    // Unsupported version in strict mode.
    assert!(
        parse_traceparent("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none(),
        "unsupported version 01 must fail-open"
    );

    // Non-hex characters inside the trace-id segment.
    assert!(
        parse_traceparent("00-GGGG2f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none(),
        "non-hex trace-id must fail-open"
    );

    // Wrong total length (trace-id is 31 chars instead of 32).
    let short = format!("00-{}-00f067aa0ba902b7-01", "a".repeat(31));
    assert!(
        parse_traceparent(&short).is_none(),
        "short trace-id must fail-open"
    );

    // All-zero trace-id is reserved per W3C.
    assert!(
        parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none(),
        "all-zero trace-id is reserved"
    );

    // Upper-case hex must fail — W3C spec requires lowercase.
    assert!(
        parse_traceparent("00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01").is_none(),
        "uppercase hex must fail-open"
    );
}

#[test]
fn new_trace_id_is_32_hex_lowercase() {
    let t = new_trace_id();
    assert_eq!(t.0.len(), 32, "trace_id must be exactly 32 chars");
    assert!(
        t.0.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "trace_id must be lowercase hex, got: {}",
        t.0
    );
}

// ── Middleware behavior via tower::oneshot ────────────────────────────────────

fn is_lowercase_hex_32(s: &str) -> bool {
    s.len() == 32
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[tokio::test]
async fn middleware_generates_id_when_header_missing() {
    let app = build_app();
    let req = Request::builder()
        .uri("/echo")
        .body(Body::empty())
        .expect("build req");
    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let tid = resp
        .headers()
        .get("X-Trace-Id")
        .expect("X-Trace-Id present")
        .to_str()
        .expect("ascii header")
        .to_owned();
    assert!(
        is_lowercase_hex_32(&tid),
        "generated trace_id must be 32-hex lowercase, got {tid}"
    );
}

#[tokio::test]
async fn middleware_preserves_valid_traceparent() {
    let app = build_app();
    let req = Request::builder()
        .uri("/echo")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .expect("build req");
    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let tid = resp
        .headers()
        .get("X-Trace-Id")
        .expect("X-Trace-Id present")
        .to_str()
        .expect("ascii header");
    assert_eq!(tid, "4bf92f3577b34da6a3ce929d0e0e4736");
}

#[tokio::test]
async fn middleware_fails_open_on_malformed() {
    let app = build_app();
    let req = Request::builder()
        .uri("/echo")
        .header("traceparent", "xxx-not-a-valid-traceparent")
        .body(Body::empty())
        .expect("build req");
    let resp = app.oneshot(req).await.expect("oneshot");
    // Fail-open: 200 OK with a freshly generated trace_id, NOT 400.
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "malformed traceparent must NOT return 400 — fail-open"
    );
    let tid = resp
        .headers()
        .get("X-Trace-Id")
        .expect("X-Trace-Id present")
        .to_str()
        .expect("ascii header")
        .to_owned();
    assert!(
        is_lowercase_hex_32(&tid),
        "fallback trace_id must be 32-hex lowercase, got {tid}"
    );
    assert_ne!(
        tid, "xxx-not-a-valid-traceparent",
        "malformed input must NOT be reflected back as trace_id"
    );
}
