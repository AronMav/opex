use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::IntoResponse,
};
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

// Phase 62 RES-04: rate limiter types live in a leaf submodule so the lib
// facade (lib.rs) can re-export them for integration tests without pulling
// in the full gateway handler subtree (crate::gateway::handlers::auth).
// These re-exports preserve the `middleware::{AuthRateLimiter, RequestRateLimiter}`
// path consumed by `gateway/mod.rs`.
pub use super::rate_limiter::{AuthRateLimiter, RequestRateLimiter};

// ── Phase 66 REF-06 — rate-limiter size accessor ─────────────────────────
//
// The auth + request rate limiters are now stored in `gateway::mod.rs` as
// `OnceLock<Arc<T>>` statics (replacing the Phase 65 `OnceLock<&'static T>`
// backed by `Box::leak`). The `/api/health/dashboard` handler consumes this
// public async helper via `crate::gateway::middleware::rate_limiter_sizes()`;
// internally it delegates to the mod.rs accessors (`auth_limiter()`,
// `request_limiter()`) which lazily return a cheap `Arc::clone`.
//
// The Phase 65-04 `install_rate_limiter_handles` shim is retired — the
// dashboard reads sizes directly from the Arc-owned instances held in
// `gateway::mod.rs` state.

/// Phase 66 REF-06: snapshot both rate-limiter map sizes for
/// `/api/health/dashboard`. Returns `(0, 0)` before the router has
/// constructed the limiters (only happens during early startup / tests
/// that do not construct a gateway). Each call takes the limiter's async
/// lock briefly — the background sweeper (Phase 62 RES-04) keeps the maps
/// bounded, so this is an O(1)-wall-clock read in practice.
pub async fn rate_limiter_sizes() -> (u64, u64) {
    let auth_size = match super::auth_limiter_opt() {
        Some(lim) => lim.snapshot_size().await as u64,
        None => 0,
    };
    let req_size = match super::request_limiter_opt() {
        Some(lim) => lim.snapshot_size().await as u64,
        None => 0,
    };
    (auth_size, req_size)
}

/// Per-IP budget for concurrent WebSocket upgrades (pre-auth).
/// Prevents `DoS` via mass WS upgrade requests before auth is checked.
/// NOTE: currently bypassed because the budget is released on 101 response, not on WS close.
/// Kept for future use when proper connection-lifetime tracking is implemented.
#[allow(dead_code)]
pub(crate) struct WsConnectionBudget {
    max_per_ip: u32,
    /// IP → active connection count
    counts: Mutex<HashMap<String, u32>>,
}

#[allow(dead_code)]
impl WsConnectionBudget {
    pub(crate) fn new(max_per_ip: u32) -> Self {
        Self { max_per_ip, counts: Mutex::new(HashMap::new()) }
    }

    pub(crate) async fn acquire(&self, ip: &str) -> bool {
        let mut counts = self.counts.lock().await;
        let count = counts.entry(ip.to_string()).or_insert(0);
        if *count >= self.max_per_ip {
            return false;
        }
        *count += 1;
        true
    }

    pub(crate) async fn release(&self, ip: &str) {
        let mut counts = self.counts.lock().await;
        if let Some(count) = counts.get_mut(ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(ip);
            }
        }
    }
}

/// Phase 64 SEC-05: dedicated per-IP rate limiter for `/api/csp-report`.
///
/// Runs BEFORE the CSP handler so over-quota requests never hit the metrics
/// counter or the JSON extractor. Returns 429 with `Retry-After` when the
/// bucket is drained.
///
/// This middleware is additive to the global 300 rpm `RequestRateLimiter`.
/// Both apply: the global limiter protects the Pi from overall overload,
/// while this limiter prevents one anonymous IP from flooding the CSP path
/// (which is in `PUBLIC_PREFIX` so it bypasses the 500-fails-in-30s auth
/// lockout).
pub(crate) async fn csp_report_rate_limit_middleware(
    req: Request<Body>,
    next: Next,
    limiter: Arc<crate::gateway::handlers::csp::CspReportRateLimiter>,
) -> axum::response::Response {
    let path = req.uri().path();
    if path != "/api/csp-report" {
        return next.run(req).await;
    }
    // Loopback callers (internal / test) bypass the limiter.
    let client_ip = extract_client_ip(&req);
    if is_loopback(&client_ip) {
        return next.run(req).await;
    }
    let Ok(parsed) = client_ip.parse::<std::net::IpAddr>() else {
        // Unknown peer address — let it through; the global limiter still
        // applies and unknown-origin should be rare.
        return next.run(req).await;
    };
    if let Some(status) =
        crate::gateway::handlers::csp::csp_report_rate_limit(&limiter, parsed)
    {
        let mut response = (status, "csp-report rate limit exceeded").into_response();
        response
            .headers_mut()
            .insert("Retry-After", "60".parse().expect("integer header"));
        return response;
    }
    next.run(req).await
}

pub(crate) async fn request_rate_limit_middleware(
    req: Request<Body>,
    next: Next,
    limiter: Arc<RequestRateLimiter>,
    _ws_budget: Arc<WsConnectionBudget>,
) -> impl IntoResponse {
    let path = req.uri().path();
    // Exempt health from rate limiting
    if path == "/health" {
        return next.run(req).await;
    }

    // Skip WS budget for upgrade requests — the budget is released on 101 response,
    // not when the WS connection actually closes. The auth middleware provides sufficient protection.
    if path.starts_with("/ws") {
        return next.run(req).await;
    }

    let client_ip = extract_client_ip(&req);

    // Exempt loopback from request rate limiting (internal services: toolgate, channels, engine)
    if is_loopback(&client_ip) {
        return next.run(req).await;
    }

    match limiter.check(&client_ip).await {
        Ok(()) => next.run(req).await,
        Err(retry_after) => {
            tracing::warn!(ip = %client_ip, "rate limited: {} req/min exceeded", limiter.max_per_minute);
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                format!("Rate limit exceeded. Retry after {retry_after}s."),
            ).into_response();
            response.headers_mut().insert(
                "Retry-After",
                retry_after.to_string().parse().expect("integer is valid header value"),
            );
            response
        }
    }
}

pub(crate) fn extract_client_ip(req: &Request<Body>) -> String {
    // Use actual TCP peer address (ConnectInfo) — not spoofable.
    // X-Forwarded-For/X-Real-IP are ignored because there is no trusted reverse proxy.
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map_or_else(|| "unknown".to_string(), |ci| ci.0.ip().to_string())
}

/// Check if an IP string represents a loopback address.
/// Handles: "127.0.0.1", "`::1`", "`::ffff:127.0.0.1`".
/// "unknown" (missing `ConnectInfo`) is NOT treated as loopback — unknown origin must authenticate.
pub(crate) fn is_loopback(ip: &str) -> bool {
    ip == "127.0.0.1" || ip == "::1" || ip.starts_with("::ffff:127.")
}

pub(crate) async fn auth_middleware(
    req: Request<Body>,
    next: Next,
    expected_token: Arc<str>,
    rate_limiter: Arc<AuthRateLimiter>,
    ws_tickets: Arc<Mutex<HashMap<String, std::time::Instant>>>,
) -> impl IntoResponse {
    let path = req.uri().path();

    // ── Public paths (no auth required) ──────────────────────────────
    // /health              — liveness probe
    // /webhook/*           — per-endpoint auth (HMAC signatures)
    // /uploads/*           — UUID filenames, no secrets
    // /api/oauth/callback  — browser redirect from OAuth provider
    // /api/triggers/email/push — validates ?token= query param internally
    // /api/csp-report      — browsers cannot authenticate CSP reports;
    //                        dedicated per-IP limiter in
    //                        `csp_report_rate_limit_middleware` protects it
    //                        from anonymous abuse (Phase 64 SEC-05).
    const PUBLIC_EXACT: &[&str] = &[
        "/health",
        "/api/oauth/callback",
        "/api/triggers/email/push",
        "/api/csp-report",
    ];
    const PUBLIC_PREFIX: &[&str] = &["/webhook/", "/uploads/", "/workspace-files/"];

    if PUBLIC_EXACT.contains(&path) || PUBLIC_PREFIX.iter().any(|p| path.starts_with(p)) {
        return next.run(req).await;
    }

    let client_ip = extract_client_ip(&req);
    tracing::debug!(ip = %client_ip, path = %path, loopback = is_loopback(&client_ip), "auth middleware");

    // ── Loopback-only paths (internal service calls) ─────────────────
    // /api/mcp/callback    — MCP server callbacks
    // /api/channels/notify — watchdog/internal alerts
    // /api/media/upload    — toolgate media uploads
    // /uploads/*           — static file serving
    // /ws*                 — WebSocket (validated separately via ticket)
    if is_loopback(&client_ip) {
        const LOOPBACK_EXACT: &[&str] = &["/health", "/api/mcp/callback", "/api/channels/notify", "/api/media/upload"];
        const LOOPBACK_PREFIX: &[&str] = &["/uploads/", "/workspace-files/", "/ws"];
        let loopback_allowed = LOOPBACK_EXACT.contains(&path)
            || LOOPBACK_PREFIX.iter().any(|p| path.starts_with(p));
        if loopback_allowed {
            return next.run(req).await;
        }
        // All other loopback requests must still provide a valid auth token
    }

    let exempt_from_lockout = is_loopback(&client_ip);

    // Check Authorization header BEFORE lockout — a valid token always passes and clears lockout.
    // This prevents locking out legitimate users who accumulated failures (e.g. during login page reload).
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    if let Some(header) = auth_header
        && let Some(token) = header.strip_prefix("Bearer ")
        && token.as_bytes().ct_eq(expected_token.as_bytes()).into() {
            rate_limiter.record_success(&client_ip).await;
            return next.run(req).await;
        }

    // Only block fully unauthenticated requests (no header at all) from locked IPs.
    // If the request HAS an Authorization header (even invalid), return 401 not 429 —
    // the frontend will redirect to login, and the next valid-token request clears lockout.
    if !exempt_from_lockout && rate_limiter.is_locked(&client_ip).await && auth_header.is_none() {
        tracing::warn!(ip = %client_ip, path = %path, "auth rate limit: locked (no auth header)");
        return (StatusCode::TOO_MANY_REQUESTS, "Too many failed attempts. Try again later.").into_response();
    }

    // For WebSocket paths, also accept ?ticket= (one-time) or ?token= (legacy) query parameter
    // (browser WebSocket API cannot set custom headers)
    if path.starts_with("/ws")
        && let Some(query) = req.uri().query() {
            for pair in query.split('&') {
                // One-time ticket (preferred — avoids exposing static token in URL/logs)
                if let Some(val) = pair.strip_prefix("ticket=")
                    && crate::gateway::handlers::auth::validate_ws_ticket(&ws_tickets, val).await {
                        rate_limiter.record_success(&client_ip).await;
                        return next.run(req).await;
                    }
                // Legacy token= removed — use ticket= instead
            }
        }

    // Don't lock loopback — internal services must not be locked out.
    // Don't count static asset failures — browsers preflight these without tokens.
    let is_static_asset = path.starts_with("/_next/") || path.ends_with(".js") || path.ends_with(".css")
        || path.ends_with(".png") || path.ends_with(".jpg") || path.ends_with(".ico") || path.ends_with(".svg")
        || path.ends_with(".woff2") || path.starts_with("/api/setup/");
    if !exempt_from_lockout && !is_static_asset {
        rate_limiter.record_failure(&client_ip).await;
    }
    StatusCode::UNAUTHORIZED.into_response()
}
