//! Phase 64 SEC-05 — CSP violation report collector (pure core).
//!
//! Mode v0.19.0: **Report-Only**. This module ONLY collects + counts; no
//! enforcement. Flip to `Content-Security-Policy` is deferred to v0.19.1
//! after the 7-day observation window.
//!
//! Abuse hardening (Warnings 9 + 10 from cross-AI review):
//!   * 64 KiB body cap at the route layer ([`CSP_REPORT_MAX_BODY`]) — real
//!     browser reports are <4 KiB; anything larger is abuse.
//!   * Per-IP rate limiter (~30 rpm) via [`CspReportRateLimiter`] — because
//!     `/api/csp-report` is in `PUBLIC_PREFIX` (browsers cannot authenticate
//!     CSP reports), a dedicated limiter closes the abuse vector without
//!     touching the global 300 rpm limiter that legitimate API traffic uses.
//!   * Directive-key length cap (`MAX_CSP_DIRECTIVE_LEN`) and map cardinality
//!     cap (`MAX_CSP_DIRECTIVES`) in `MetricsRegistry::record_csp_violation`
//!     prevent counter-map bloat.
//!
//! Leaf-module discipline: this module depends ONLY on `axum`, `serde`,
//! `std`, `tracing`, and `crate::metrics::MetricsRegistry` — no `AppState`
//! cascade. That lets `lib.rs` re-export it at path
//! `hydeclaw_core::gateway::csp` for integration tests.
//!
//! Protocol note: browsers POST the legacy `application/csp-report` format
//! (`{"csp-report": {...}}`). The modern Reporting API
//! `application/reports+json` batch format is not yet accepted here.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde::Deserialize;

use crate::metrics::MetricsRegistry;

#[cfg(test)]
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    response::IntoResponse,
    routing::post,
};

/// Route-level body-size cap: 64 KiB. Real CSP reports are <4 KiB; anything
/// bigger is an abuse signal and returns 413 Payload Too Large via Axum's
/// `DefaultBodyLimit` layer.
pub const CSP_REPORT_MAX_BODY: usize = 64 * 1024;

/// Per-IP rate limit for `/api/csp-report` in requests per minute. Dedicated
/// (separate from the global 300 rpm limiter) because adding the path to
/// `PUBLIC_PREFIX` bypasses the 500-fails-in-30s auth lockout.
pub const CSP_REPORT_PER_IP_RPM: u32 = 30;

/// Evict per-IP buckets idle for longer than this window. Prevents the
/// rate-limiter map itself from growing unbounded under attack.
const CSP_REPORT_IDLE_EVICT_SECS: u64 = 5 * 60;

// ── Report body shape ───────────────────────────────────────────────────────

/// Browser CSP violation report body, `application/csp-report` legacy format.
#[derive(Deserialize, Debug, Default)]
pub struct CspReportBody {
    #[serde(rename = "violated-directive", default)]
    pub violated_directive: String,
    #[serde(rename = "blocked-uri", default)]
    pub blocked_uri: String,
    #[serde(rename = "document-uri", default)]
    pub document_uri: String,
    #[serde(rename = "effective-directive", default)]
    pub effective_directive: String,
}

/// Wrapper — the browser always sends `{"csp-report": {...}}`.
#[derive(Deserialize, Debug, Default)]
pub struct CspReport {
    #[serde(rename = "csp-report", default)]
    pub report: CspReportBody,
}

// ── Handler core ────────────────────────────────────────────────────────────

/// Extract the canonical directive name from a report body. Prefers
/// `effective-directive` (modern browsers) and falls back to
/// `violated-directive` (older browsers) — in both cases we take the FIRST
/// whitespace-delimited token so full policy strings (e.g. "script-src 'self'")
/// do not bleed into counter keys.
fn extract_directive(body: &CspReportBody) -> String {
    let source = if !body.effective_directive.is_empty() {
        body.effective_directive.as_str()
    } else {
        body.violated_directive.as_str()
    };
    source
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// Pure business logic: record a violation against the registry and return
/// the HTTP status. Split out so integration tests can call it directly
/// without constructing a full `AppState`.
pub fn api_csp_report_handler(
    metrics: &Arc<MetricsRegistry>,
    payload: CspReport,
) -> StatusCode {
    let directive = extract_directive(&payload.report);

    tracing::warn!(
        directive = %directive,
        blocked_uri = %payload.report.blocked_uri,
        document_uri = %payload.report.document_uri,
        "CSP violation (report-only)"
    );

    // record_csp_violation enforces MAX_CSP_DIRECTIVE_LEN + MAX_CSP_DIRECTIVES internally.
    metrics.record_csp_violation(&directive);

    StatusCode::NO_CONTENT
}

/// Parse a raw request body into a [`CspReport`] and dispatch to
/// [`api_csp_report_handler`]. Returns 400 on unparseable JSON so malformed
/// reports do NOT bump the counter.
///
/// Why not rely on the `axum::Json` extractor: browsers POST CSP violations
/// with `Content-Type: application/csp-report`, which the `Json` extractor
/// rejects with 415 Unsupported Media Type. Accepting any body that parses
/// as valid JSON matches browser behaviour (Firefox, Chrome, Safari all use
/// the `application/csp-report` content-type).
pub fn api_csp_report_bytes_handler(
    metrics: &Arc<MetricsRegistry>,
    body: &[u8],
) -> StatusCode {
    match serde_json::from_slice::<CspReport>(body) {
        Ok(report) => api_csp_report_handler(metrics, report),
        Err(_) => StatusCode::BAD_REQUEST,
    }
}

// ── Test-facing router ──────────────────────────────────────────────────────

/// Test-facing router factory — builds a minimal router that just wires the
/// handler against a fresh `MetricsRegistry`. No auth, no DB, no rate limiter
/// — those are exercised in dedicated tests. The body-size cap IS applied
/// (it lives on the route layer).
///
/// Consumed only by `tests/integration_csp_report.rs` (re-exported via lib.rs).
#[cfg(test)]
pub fn routes_for_test(metrics: Arc<MetricsRegistry>) -> Router {
    Router::new()
        .route("/api/csp-report", post(api_csp_report_test))
        .layer(DefaultBodyLimit::max(CSP_REPORT_MAX_BODY))
        .with_state(metrics)
}

/// Test-facing axum handler — extracts a bare `Arc<MetricsRegistry>` from
/// state for `routes_for_test`. Accepts any content-type (matches production).
#[cfg(test)]
async fn api_csp_report_test(
    State(metrics): State<Arc<MetricsRegistry>>,
    body: Bytes,
) -> impl IntoResponse {
    api_csp_report_bytes_handler(&metrics, &body)
}

// ── Per-IP rate limiter ─────────────────────────────────────────────────────

/// Dedicated per-IP rate limiter for `/api/csp-report`. Uses a fixed
/// 60-second window keyed by source IP.
///
/// Why separate from the global `RequestRateLimiter`:
///   * `/api/csp-report` lives in `PUBLIC_PREFIX` (no auth) — without a
///     dedicated limiter anonymous abuse is unbounded until the global
///     300 rpm limiter fires, and the global limiter's counters could be
///     exhausted by legitimate API traffic.
///   * Separate tuning: 30 rpm is generous for a single browser firing
///     multiple violations on one page load, but much stricter than the
///     global cap.
///
/// Eviction: entries idle for more than [`CSP_REPORT_IDLE_EVICT_SECS`] are
/// removed on every [`CspReportRateLimiter::check`] call (cheap, keeps the
/// map bounded without a background sweeper).
pub struct CspReportRateLimiter {
    buckets: Mutex<std::collections::HashMap<IpAddr, Bucket>>,
}

#[derive(Clone, Copy)]
struct Bucket {
    tokens: u32,
    window_start: Instant,
    last_seen: Instant,
}

impl CspReportRateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Returns `true` if the request is allowed, `false` if it must be
    /// rejected with 429. A fresh bucket is created on first contact.
    pub fn check(&self, ip: IpAddr) -> bool {
        let mut buckets = self.buckets.lock().expect("csp limiter poisoned");
        let now = Instant::now();

        // Cheap idle eviction — bounded since we only scan when a request
        // is being checked.
        let idle = Duration::from_secs(CSP_REPORT_IDLE_EVICT_SECS);
        buckets.retain(|_, b| now.duration_since(b.last_seen) < idle);

        let entry = buckets.entry(ip).or_insert(Bucket {
            tokens: 0,
            window_start: now,
            last_seen: now,
        });

        // Roll the window if it expired.
        if now.duration_since(entry.window_start) >= Duration::from_secs(60) {
            entry.tokens = 0;
            entry.window_start = now;
        }
        entry.last_seen = now;

        if entry.tokens >= CSP_REPORT_PER_IP_RPM {
            return false;
        }
        entry.tokens += 1;
        true
    }
}

impl Default for CspReportRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Middleware helper. Returns `Some(StatusCode::TOO_MANY_REQUESTS)` when the
/// bucket is drained for `ip`; `None` when the request should pass through.
/// Kept separate from any middleware closure so it can be unit-tested.
pub fn csp_report_rate_limit(
    limiter: &CspReportRateLimiter,
    ip: IpAddr,
) -> Option<StatusCode> {
    if limiter.check(ip) {
        None
    } else {
        tracing::warn!(ip = %ip, "csp-report rate limit: {} rpm exceeded", CSP_REPORT_PER_IP_RPM);
        Some(StatusCode::TOO_MANY_REQUESTS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_directive_prefers_effective_over_violated() {
        let body = CspReportBody {
            effective_directive: "script-src".into(),
            violated_directive: "default-src 'self'".into(),
            ..Default::default()
        };
        assert_eq!(extract_directive(&body), "script-src");
    }

    #[test]
    fn extract_directive_falls_back_to_violated() {
        let body = CspReportBody {
            effective_directive: String::new(),
            violated_directive: "img-src data:".into(),
            ..Default::default()
        };
        assert_eq!(extract_directive(&body), "img-src");
    }

    #[test]
    fn extract_directive_unknown_when_empty() {
        let body = CspReportBody::default();
        assert_eq!(extract_directive(&body), "unknown");
    }

    #[test]
    fn limiter_fresh_ip_has_full_budget() {
        let limiter = CspReportRateLimiter::new();
        let ip: IpAddr = "203.0.113.1".parse().unwrap();
        for _ in 0..CSP_REPORT_PER_IP_RPM {
            assert!(limiter.check(ip));
        }
        assert!(!limiter.check(ip), "capped after {CSP_REPORT_PER_IP_RPM}");
    }
}
