//! Phase 64 SEC-05 — CSP violation report collector (production wiring).
//!
//! The pure, `AppState`-free core (types, constants, rate limiter, handler
//! function) lives in `crate::gateway::csp_core` so integration tests can
//! import it via the `lib.rs` facade without cascading the gateway handler
//! subtree. This file is the thin production glue: it extracts
//! [`InfraServices`] and forwards to the core handler, and exports the
//! production `routes()` used in `gateway::mod.rs::router()`.

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    response::IntoResponse,
    routing::post,
};

use super::super::AppState;
use crate::gateway::clusters::InfraServices;
use crate::gateway::csp_core::{api_csp_report_bytes_handler, CSP_REPORT_MAX_BODY};

// Re-export the pieces of the pure core that the gateway `router()` needs to
// wire the middleware layer (`CspReportRateLimiter` is boxed and leaked there).
pub use crate::gateway::csp_core::{csp_report_rate_limit, CspReportRateLimiter};

/// Production router — composed into the gateway via `.merge()` in
/// `gateway/mod.rs`. Applies the 64 KiB body cap at the route layer.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/csp-report", post(api_csp_report))
        .layer(DefaultBodyLimit::max(CSP_REPORT_MAX_BODY))
}

/// Production axum handler — accepts any Content-Type (browsers POST
/// `application/csp-report`, which the `Json` extractor would reject as 415).
/// Extracts `InfraServices` and delegates to the pure core handler.
/// Returns 204 on valid JSON, 400 on malformed body.
pub(crate) async fn api_csp_report(
    State(infra): State<InfraServices>,
    body: Bytes,
) -> impl IntoResponse {
    api_csp_report_bytes_handler(&infra.metrics, &body)
}
