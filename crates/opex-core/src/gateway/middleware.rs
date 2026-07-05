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

/// Snapshot both rate-limiter map sizes for `/api/health/dashboard`.
/// Returns `(0, 0)` before the router has constructed the limiters
/// (early startup or tests that skip the gateway). Each call takes the
/// limiter's async lock briefly — the background sweeper keeps the maps
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

/// Dedicated per-IP rate limiter for `/webhook/*`.
///
/// Webhooks bypass auth (they are in `PUBLIC_PREFIX` and validate themselves
/// via HMAC signatures), so without a dedicated limiter a noisy webhook
/// source could exhaust the global 300 rpm `RequestRateLimiter` shared
/// with other anonymous endpoints (`/health`, `/api/oauth/callback`,
/// `/api/triggers/email/push`, `/api/csp-report`, `/api/uploads/*`,
/// `/workspace-files/*`).
///
/// Additive to the global limiter — both apply. Loopback callers (internal
/// services) bypass this limiter; the global one already exempts them.
pub(crate) async fn webhook_rate_limit_middleware(
    req: Request<Body>,
    next: Next,
    limiter: Arc<RequestRateLimiter>,
) -> axum::response::Response {
    let path = req.uri().path();
    if !path.starts_with("/webhook/") {
        return next.run(req).await;
    }
    let client_ip = extract_client_ip(&req);
    if is_loopback(&client_ip) {
        return next.run(req).await;
    }
    match limiter.check(&client_ip).await {
        Ok(()) => next.run(req).await,
        Err(retry_after) => {
            tracing::warn!(
                ip = %client_ip,
                "webhook rate limit: {} req/min exceeded",
                limiter.max_per_minute
            );
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                format!("Webhook rate limit exceeded. Retry after {retry_after}s."),
            ).into_response();
            response.headers_mut().insert(
                "Retry-After",
                retry_after.to_string().parse().expect("integer is valid header value"),
            );
            response
        }
    }
}

pub(crate) async fn request_rate_limit_middleware(
    req: Request<Body>,
    next: Next,
    limiter: Arc<RequestRateLimiter>,
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

/// Check if an IP is a Docker bridge gateway (host-gateway).
///
/// Docker bridge networks use the 172.16.0.0/12 range by default; the gateway
/// is typically 172.17.0.1 or 172.18.0.1. `host.docker.internal:host-gateway`
/// resolves to this IP from inside the container. These IPs are not loopback
/// but are internal to the Docker host — safe enough for codemode endpoints
/// which are additionally protected by the X-Codemode-Token HMAC.
pub(crate) fn is_docker_gateway(ip: &str) -> bool {
    // Strip IPv4-mapped IPv6 prefix.
    let ip = ip.strip_prefix("::ffff:").unwrap_or(ip);
    // Match 172.16.0.0/12 — exactly four dotted octets where the first is 172
    // and the second is in [16, 31]. All four octets must be valid u8.
    let mut parts = ip.split('.');
    let (Some(a), Some(b), Some(c), Some(d)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    a == "172"
        && (16..=31).contains(&(b.parse::<u8>().unwrap_or(0)))
        && c.parse::<u8>().is_ok()
        && d.parse::<u8>().is_ok()
        && parts.next().is_none()
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
    // /api/uploads/*       — HMAC-signed read endpoint for DB-backed binary assets
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
    // /api/shares/*         — read-only shared session snapshot; the unguessable
    //                         token in the path is the security boundary
    const PUBLIC_PREFIX: &[&str] = &["/webhook/", "/api/uploads/", "/workspace-files/", "/api/shares/"];

    if PUBLIC_EXACT.contains(&path) || PUBLIC_PREFIX.iter().any(|p| path.starts_with(p)) {
        return next.run(req).await;
    }

    let client_ip = extract_client_ip(&req);
    tracing::debug!(ip = %client_ip, path = %path, loopback = is_loopback(&client_ip), "auth middleware");

    // ── Loopback-only paths (internal service calls) ─────────────────
    // /api/channels/notify — watchdog/internal alerts
    // /api/media/upload    — toolgate media uploads
    // /api/vision/analyze  — vision proxy called by the analyze_image capability tool
    // /api/uploads/*       — DB-backed binary read endpoint
    //
    // /ws* is intentionally NOT free-passed here even on loopback. Audit
    // 2026-05-08 found that any local process (toolgate, MCP container with
    // host networking, RCE through a YAML tool) could connect as a channel
    // adapter without a ticket. The TypeScript channel adapter already
    // fetches a one-time ticket via POST /api/auth/ws-ticket, so requiring
    // the ticket on loopback breaks nothing legitimate.
    if is_loopback(&client_ip) {
        const LOOPBACK_EXACT: &[&str] = &[
            "/health",
            "/api/channels/notify",
            "/api/media/upload",
            "/api/vision/analyze",
            // Codemode (tools-as-code): sandbox scripts call back into core to
            // invoke tools and search the tool catalog. Security boundary is
            // the X-Codemode-Token HMAC (verified in the handler), not the
            // bearer token — loopback-only so a remote attacker can't reach it.
            "/api/sandbox/tool-call",
            "/api/sandbox/tool-search",
        ];
        const LOOPBACK_PREFIX: &[&str] = &["/api/uploads/"];
        let loopback_allowed = LOOPBACK_EXACT.contains(&path)
            || LOOPBACK_PREFIX.iter().any(|p| path.starts_with(p));
        if loopback_allowed {
            return next.run(req).await;
        }
        // All other loopback requests must still provide a valid auth token
        // or (for /ws*) a valid one-time ticket.
    }

    // Codemode sandbox containers reach core via the Docker bridge gateway
    // (host.docker.internal → host-gateway), which is NOT a loopback IP
    // (typically 172.17.0.1 or 172.18.0.1). Allow these Docker-internal IPs
    // for the codemode endpoints only — they are still protected by the
    // X-Codemode-Token HMAC, and a remote attacker cannot reach them.
    if is_docker_gateway(&client_ip)
        && (path == "/api/sandbox/tool-call" || path == "/api/sandbox/tool-search")
    {
        return next.run(req).await;
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
        || path.ends_with(".woff2");
    // First-run setup wizard: failures here should NOT count toward the auth
    // lockout — the browser legitimately probes /api/setup/status before any
    // token exists. Kept separate from `is_static_asset` so the bypass list
    // doesn't grow to include arbitrary API paths the next time someone
    // adds a new pre-auth endpoint here.
    let is_setup_wizard = path.starts_with("/api/setup/");
    if !exempt_from_lockout && !is_static_asset && !is_setup_wizard {
        rate_limiter.record_failure(&client_ip).await;
    }
    StatusCode::UNAUTHORIZED.into_response()
}

/// Sanitize 500 response bodies: log the original detail server-side and
/// replace the client-visible body with a generic JSON error.
///
/// Internal error strings routinely carry SQL fragments, filesystem paths
/// and upstream URLs (`ApiError::Internal(e.to_string())` and the ~100 raw
/// `(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())` handler sites) —
/// none of that belongs in an HTTP response. One choke point here beats
/// sweeping every call site.
///
/// Scope is exactly `500 Internal Server Error`: 503s carry intentional
/// structured bodies (e.g. toolgate `{"degraded": true}`) and stay intact.
/// Response headers are preserved (only content-type/length are rewritten),
/// so CORS / security / trace headers added by outer layers are unaffected.
pub(crate) async fn sanitize_internal_error_middleware(
    req: Request<Body>,
    next: Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let response = next.run(req).await;
    if response.status() != StatusCode::INTERNAL_SERVER_ERROR {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    // 64 KiB cap: 500 bodies are short error strings; an oversized body is
    // dropped rather than buffered in full.
    let detail = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => "<unreadable 500 body>".to_owned(),
    };
    tracing::error!(method = %method, path = %path, detail = %detail, "internal error response sanitized");
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    let body = Body::from(r#"{"error":"internal server error"}"#);
    axum::response::Response::from_parts(parts, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    // ── sanitize_internal_error_middleware tests ────────────────────────
    mod sanitize_500 {
        use super::super::sanitize_internal_error_middleware;
        use axum::Router;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::response::IntoResponse;
        use axum::routing::get;
        use tower::ServiceExt;

        fn app() -> Router {
            Router::new()
                .route(
                    "/boom",
                    get(|| async {
                        let mut resp = (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "db error: relation \"secret_table\" does not exist at /home/op/x.rs",
                        )
                            .into_response();
                        resp.headers_mut()
                            .insert("x-custom", "kept".parse().unwrap());
                        resp
                    }),
                )
                .route(
                    "/degraded",
                    get(|| async {
                        (StatusCode::SERVICE_UNAVAILABLE, r#"{"degraded":true}"#).into_response()
                    }),
                )
                .route("/ok", get(|| async { "fine" }))
                .layer(axum::middleware::from_fn(sanitize_internal_error_middleware))
        }

        async fn body_string(resp: axum::response::Response) -> String {
            let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                .await
                .expect("body");
            String::from_utf8_lossy(&bytes).into_owned()
        }

        #[tokio::test]
        async fn replaces_500_body_with_generic_json() {
            let resp = app()
                .oneshot(Request::get("/boom").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(
                resp.headers().get("content-type").unwrap(),
                "application/json"
            );
            // Non-content headers survive the rewrite (CORS/trace analogue).
            assert_eq!(resp.headers().get("x-custom").unwrap(), "kept");
            let body = body_string(resp).await;
            assert_eq!(body, r#"{"error":"internal server error"}"#);
            assert!(!body.contains("secret_table"));
        }

        #[tokio::test]
        async fn leaves_non_500_responses_intact() {
            let resp = app()
                .oneshot(Request::get("/ok").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(body_string(resp).await, "fine");

            let resp = app()
                .oneshot(Request::get("/degraded").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body_string(resp).await, r#"{"degraded":true}"#);
        }
    }

    // ── is_loopback tests ───────────────────────────────────────────────
    #[test]
    fn is_loopback_accepts_ipv4_127_0_0_1() {
        assert!(is_loopback("127.0.0.1"));
    }

    #[test]
    fn is_loopback_accepts_ipv6_localhost() {
        assert!(is_loopback("::1"));
    }

    #[test]
    fn is_loopback_accepts_ipv6_mapped_ipv4_loopback() {
        assert!(is_loopback("::ffff:127.0.0.1"));
    }

    #[test]
    fn is_loopback_accepts_ipv6_mapped_ipv4_loopback_variant() {
        assert!(is_loopback("::ffff:127.255.255.255"));
    }

    #[test]
    fn is_loopback_rejects_public_ipv4_8_8_8_8() {
        assert!(!is_loopback("8.8.8.8"));
    }

    #[test]
    fn is_loopback_rejects_private_lan_192_168() {
        assert!(!is_loopback("192.168.1.85"));
    }

    #[test]
    fn is_loopback_rejects_private_lan_10() {
        assert!(!is_loopback("10.0.0.1"));
    }

    #[test]
    fn is_loopback_rejects_unknown_string() {
        assert!(!is_loopback("unknown"));
    }

    #[test]
    fn is_loopback_rejects_empty_string() {
        assert!(!is_loopback(""));
    }

    #[test]
    fn is_loopback_rejects_ipv6_non_loopback() {
        assert!(!is_loopback("2001:db8::1"));
    }

    #[test]
    fn is_loopback_case_sensitive_rejects_uppercase() {
        // "::1" should match, but "::1" uppercase doesn't exist, so just verify exact match
        assert!(!is_loopback("::1::1")); // malformed but should not panic
    }

    // ── is_docker_gateway tests (C3: codemode loopback via Docker bridge) ──

    #[test]
    fn is_docker_gateway_accepts_default_bridge() {
        assert!(is_docker_gateway("172.17.0.1"), "default Docker bridge gateway");
        assert!(is_docker_gateway("172.18.0.1"), "custom bridge gateway");
        assert!(is_docker_gateway("172.20.0.1"));
        assert!(is_docker_gateway("172.31.0.1"), "upper bound of /12");
    }

    #[test]
    fn is_docker_gateway_accepts_ipv4_mapped_ipv6() {
        assert!(is_docker_gateway("::ffff:172.17.0.1"));
    }

    #[test]
    fn is_docker_gateway_rejects_loopback() {
        assert!(!is_docker_gateway("127.0.0.1"));
    }

    #[test]
    fn is_docker_gateway_rejects_public_ip() {
        assert!(!is_docker_gateway("8.8.8.8"));
    }

    #[test]
    fn is_docker_gateway_rejects_other_private_ranges() {
        assert!(!is_docker_gateway("10.0.0.1"));
        assert!(!is_docker_gateway("192.168.1.1"));
    }

    #[test]
    fn is_docker_gateway_rejects_outside_range() {
        assert!(!is_docker_gateway("172.15.0.1"), "just below /12");
        assert!(!is_docker_gateway("172.32.0.1"), "just above /12");
    }

    #[test]
    fn is_docker_gateway_rejects_malformed() {
        assert!(!is_docker_gateway("not-an-ip"));
        assert!(!is_docker_gateway(""));
        assert!(!is_docker_gateway("172.17"));
    }

    // ── extract_client_ip tests ─────────────────────────────────────────
    #[test]
    fn extract_client_ip_returns_unknown_when_no_connect_info() {
        let req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        let ip = extract_client_ip(&req);
        assert_eq!(ip, "unknown");
    }

    #[test]
    fn extract_client_ip_extracts_ipv4_from_connect_info() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345);
        let mut req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(socket_addr));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "127.0.0.1");
    }

    #[test]
    fn extract_client_ip_extracts_public_ipv4_from_connect_info() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 54321);
        let mut req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(socket_addr));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "8.8.8.8");
    }

    #[test]
    fn extract_client_ip_extracts_ipv6_from_connect_info() {
        use std::net::{IpAddr, Ipv6Addr, SocketAddr};

        let socket_addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 9999);
        let mut req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(socket_addr));

        let ip = extract_client_ip(&req);
        assert_eq!(ip, "::1");
    }

    #[test]
    fn extract_client_ip_ignores_x_forwarded_for_header() {
        // Per the comment in extract_client_ip, X-Forwarded-For is intentionally ignored
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345);
        let mut req = Request::builder()
            .uri("/test")
            .header("X-Forwarded-For", "8.8.8.8")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(socket_addr));

        let ip = extract_client_ip(&req);
        // Should use ConnectInfo, not the header
        assert_eq!(ip, "127.0.0.1");
    }
}
