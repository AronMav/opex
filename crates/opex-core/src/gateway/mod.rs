use axum::{
    Router,
    middleware as axum_mw,
};
use std::sync::{Arc, OnceLock};
use tower_http::services::{ServeDir, ServeFile};
use serde::{Deserialize, Serialize};

pub mod error;
// Phase 62 RES-04: rate limiter types extracted to a leaf submodule
// (zero crate:: imports) so lib.rs can re-export for integration tests.
pub use opex_gateway_util::rate_limiter;
// Phase 64 SEC-05: pure CSP report core — leaf module (deps: axum, serde,
// std, tracing, `crate::metrics::MetricsRegistry`). Re-exported from lib.rs
// at path `opex_core::gateway::csp` for integration tests.
pub mod csp_core;
// Phase 64 SEC-04: streaming body cap + struson primitives for POST /api/restore.
// Leaf module — zero `crate::*` imports — re-exported from lib.rs at path
// `opex_core::gateway::restore_stream_core` for integration_backup_size_cap.rs.
pub use opex_gateway_util::restore_stream_core;
// Phase 65 OBS-04: W3C Trace Context middleware — leaf module (axum + tracing
// + uuid, zero `crate::*` imports). Re-exported from lib.rs at path
// `opex_core::gateway::trace_context` for integration_trace_context.rs.
pub use opex_gateway_util::trace_context;
pub mod middleware;
pub mod sse;
pub mod stream_registry;
pub mod stream_jobs;
pub mod state;
pub mod clusters;
pub(crate) mod handlers;
pub use error::ApiError;
pub use state::*;
use middleware::{AuthRateLimiter, auth_middleware, RequestRateLimiter, request_rate_limit_middleware, csp_report_rate_limit_middleware, webhook_rate_limit_middleware, sanitize_internal_error_middleware};
// Re-export for use by main.rs
pub use handlers::agents::start_agent_from_config;
pub use handlers::email_triggers::renew_expiring_gmail_watches;
pub use handlers::channels::migrate_credentials_to_vault;
pub use handlers::providers::migrate_provider_keys_to_vault;
pub(crate) use handlers::backup::create_backup_internal;
pub(crate) use handlers::notifications::notify;

// Rate limiter family + shared auth token live behind `OnceLock<Arc<T>>` statics.
// Middleware closures `Arc::clone` into their captures; the strong-count stays
// bounded (~6), and dropping the router releases those Arcs — no leaked allocation.
// `/api/health/dashboard` reads sizes via `middleware::rate_limiter_sizes()`,
// which delegates to `auth_limiter_opt()` / `request_limiter_opt()` below.

static SHARED_TOKEN: OnceLock<Arc<str>> = OnceLock::new();
static AUTH_LIMITER: OnceLock<Arc<AuthRateLimiter>> = OnceLock::new();
static REQ_LIMITER: OnceLock<Arc<RequestRateLimiter>> = OnceLock::new();
static CSP_LIMITER: OnceLock<Arc<handlers::csp::CspReportRateLimiter>> = OnceLock::new();
static WEBHOOK_LIMITER: OnceLock<Arc<RequestRateLimiter>> = OnceLock::new();

/// Returns the auth rate limiter, or `None` before the router has been
/// constructed (e.g. early startup / tests that skip the gateway).
pub(crate) fn auth_limiter_opt() -> Option<Arc<AuthRateLimiter>> {
    AUTH_LIMITER.get().cloned()
}

/// Returns the request rate limiter, or `None` before the router has been constructed.
pub(crate) fn request_limiter_opt() -> Option<Arc<RequestRateLimiter>> {
    REQ_LIMITER.get().cloned()
}

/// Public OpenAI-format message — used by gateway AND referenced from `engine::handle_openai`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    /// Vercel AI SDK 3.x format: array of message parts (text, reasoning, tool calls)
    #[serde(default)]
    pub parts: Option<Vec<MessagePart>>,
}

/// Part of a message in Vercel AI SDK 3.x format
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MessagePart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default)]
    pub text: Option<String>,
}

pub fn router(state: AppState) -> anyhow::Result<Router> {
    let auth_token = state
        .config
        .config
        .gateway
        .auth_token_env
        .as_ref()
        .and_then(|env_name| std::env::var(env_name).ok());

    let app = Router::new()
        .merge(handlers::chat::routes())           // /health, /api/chat, /v1/chat/completions, /v1/models, /v1/embeddings
        .merge(handlers::auth::routes())            // /api/auth/ws-ticket
        .merge(handlers::channel_ws::routes())      // /ws, /ws/channel/{agent_name}
        .merge(handlers::agents::routes())          // /api/agents/*, /api/approvals/*
        .merge(handlers::sessions::routes())        // /api/sessions/*, /api/messages/*
        .merge(handlers::session_failures::routes())// /api/sessions/failures, /api/sessions/{id}/failures
        .merge(handlers::catalog::routes())         // /api/catalog/providers (preset picker)
        .merge(handlers::commands::routes())        // /api/commands (chat command registry)
        .merge(handlers::monitoring::routes(state.clone()))// /api/setup/*, /api/status, /api/stats, /api/usage/*, /api/doctor, /api/audit/*, /api/watchdog/*
        .merge(handlers::providers::routes())       // /api/providers/*, /api/provider-types, /api/media-drivers, /api/media-config, /api/provider-active
        .merge(handlers::network::routes())         // /api/network/addresses
        .merge(handlers::secrets::routes())         // /api/secrets/*
        .merge(handlers::memory::routes())          // /api/memory/*
        .merge(handlers::cron::routes())            // /api/cron/*
        .merge(handlers::tools::routes())           // /api/tool-definitions, /api/tools/*, /api/mcp/*
        .merge(handlers::yaml_tools::routes())      // /api/yaml-tools/*, /api/agents/*/yaml-tools/*
        .merge(handlers::skills::routes())          // /api/skills/*, /api/agents/*/skills/*
        .merge(handlers::channels::routes())        // /api/channels/*, /api/agents/*/channels/*, /api/agents/*/hooks
        .merge(handlers::config::routes())          // /api/config/*, /api/restart, /api/tts/*, /api/canvas/*
        .merge(handlers::backup::routes())          // /api/backup/*, /api/restore
        .merge(handlers::curator::routes())           // /api/curator/*
        .merge(handlers::curator_decisions::routes()) // /api/curator-decisions/*, /api/skills/*/curator-decisions
        .merge(handlers::services::routes())        // /api/services/*, /api/containers/*
        .merge(handlers::webhooks::routes())        // /api/webhooks/*, /webhook/*
        .merge(handlers::oauth::routes())           // /api/oauth/*, /api/agents/*/oauth/*
        .merge(handlers::email_triggers::routes())  // /api/triggers/email/*
        .merge(handlers::github_repos::routes())    // /api/agents/*/github/repos/*
        .merge(handlers::access::routes())          // /api/access/*
        .merge(handlers::notifications::routes())   // /api/notifications/*
        .merge(handlers::csp::routes())             // Phase 64 SEC-05: /api/csp-report (report-only)
        .merge(handlers::media::routes())           // /api/media/*
        .merge(handlers::uploads_serve::routes())   // /api/uploads/{id}
        .merge(handlers::workspace_files::routes()) // /workspace-files/{*path}?sig=&exp=
        .merge(handlers::workspace::routes())       // /api/workspace/*
        .merge(handlers::clarify::routes())        // /api/clarify/{id}
        .merge(handlers::files::routes())           // /api/files/{upload_id}/actions + /run, /api/commands/menu-run
        .merge(handlers::handlers_admin::routes())  // /api/handlers, /api/handlers/allowlist (File Handlers tab)
        .merge(handlers::llm::routes())              // /api/llm/complete (raw LLM, auth-required)
        .merge(handlers::internal_creds::routes())    // /api/internal/its-credentials (ITS 1C login, auth-required)
        .merge(handlers::youtube_creds::routes())     // /api/internal/youtube-cookies (toolgate yt-dlp cookies, auth-required)
        .merge(handlers::sandbox::routes());          // /api/sandbox/tool-call, /api/sandbox/tool-search (codemode, loopback + HMAC)

    #[cfg(feature = "gemini-cloudcode")]
    let app = app.merge(handlers::google_auth::routes()); // /api/auth/google/*

    // Auth middleware — REQUIRED. Refuse to start without a token.
    // `.set(...).ok()` is idempotent: second router construction (tests,
    // hot-reload) keeps the original Arc — no allocation delta.
    let Some(token) = auth_token else {
        tracing::error!("FATAL: no auth token configured — refusing to start unauthenticated gateway");
        tracing::error!("set gateway.auth_token_env in config and provide the env var");
        anyhow::bail!("gateway requires auth token — set gateway.auth_token_env in opex.toml");
    };
    // First-router wins; subsequent constructions reuse the same Arc.
    let _ = SHARED_TOKEN.set(Arc::from(token.as_str()));
    let _ = AUTH_LIMITER.set(Arc::new(AuthRateLimiter::new(500, 30)));
    let shared_token = SHARED_TOKEN.get().cloned().expect("SHARED_TOKEN just set");
    let rate_limiter = AUTH_LIMITER.get().cloned().expect("AUTH_LIMITER just set");

    let ws_tickets = state.auth.ws_tickets.clone();
    let app = {
        let shared_token = shared_token.clone();
        let rate_limiter = rate_limiter.clone();
        app.layer(axum_mw::from_fn(move |req, next| {
            let shared_token = shared_token.clone();
            let rate_limiter = rate_limiter.clone();
            let ws_tickets = ws_tickets.clone();
            async move { auth_middleware(req, next, shared_token, rate_limiter, ws_tickets).await }
        }))
    };

    // Request rate limiting (per-IP, from config limits.max_requests_per_minute)
    let max_rpm = state.config.config.limits.max_requests_per_minute;
    let _ = REQ_LIMITER.set(Arc::new(RequestRateLimiter::new(max_rpm)));
    let req_limiter = REQ_LIMITER.get().cloned().expect("REQ_LIMITER just set");
    let app = {
        let req_limiter = req_limiter.clone();
        app.layer(axum_mw::from_fn(move |req, next| {
            request_rate_limit_middleware(req, next, req_limiter.clone())
        }))
    };

    // Dedicated per-IP limiter on /api/csp-report (~30 rpm).
    // Additive to the global limiter above — both apply.
    let _ = CSP_LIMITER.set(Arc::new(handlers::csp::CspReportRateLimiter::new()));
    let csp_limiter = CSP_LIMITER.get().cloned().expect("CSP_LIMITER just set");
    let app = app.layer(axum_mw::from_fn(move |req, next| {
        csp_report_rate_limit_middleware(req, next, csp_limiter.clone())
    }));

    // Dedicated per-IP limiter on /webhook/* (60 rpm). Additive to the global limiter.
    // Prevents noisy webhook sources from exhausting the global 300 rpm budget
    // shared with other anonymous endpoints.
    let _ = WEBHOOK_LIMITER.set(Arc::new(RequestRateLimiter::new(60)));
    let webhook_limiter = WEBHOOK_LIMITER.get().cloned().expect("WEBHOOK_LIMITER just set");
    let app = {
        let webhook_limiter = webhook_limiter.clone();
        app.layer(axum_mw::from_fn(move |req, next| {
            webhook_rate_limit_middleware(req, next, webhook_limiter.clone())
        }))
    };

    // 500-body sanitizer: the client gets a generic JSON error; the original
    // detail (SQL, paths, upstream URLs) goes to the log with method+path.
    // Added here so it wraps every API route registered above while staying
    // inside the CORS / security-header layers (their headers are applied on
    // the way out and survive the body rewrite).
    let app = app.layer(axum_mw::from_fn(sanitize_internal_error_middleware));

    // Background sweeper tasks: every 60s evict expired rate-limiter entries.
    // Each sweeper owns its own Arc::clone — dropping the router releases it.
    {
        let rate_limiter = rate_limiter.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                rate_limiter.sweep().await;
            }
        });
    }
    {
        let req_limiter = req_limiter.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                req_limiter.sweep().await;
            }
        });
    }
    {
        let webhook_limiter = webhook_limiter.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                webhook_limiter.sweep().await;
            }
        });
    }

    // CORS: restrict to configured origins or derive from listen address.
    let cors_origins: Vec<axum::http::HeaderValue> = if state.config.config.gateway.cors_origins.is_empty() {
        // Derive from listen address: allow UI on same host (:5173) + API port
        let host = state.config.config.gateway.listen.split(':').next().unwrap_or("0.0.0.0");
        let port = state.config.config.gateway.listen.rsplit(':').next().unwrap_or("18789");
        let mut origins = vec![
            format!("http://{host}:{port}").parse().expect("valid CORS origin"),
            format!("http://{host}:5173").parse().expect("valid CORS origin"),
        ];
        // For 0.0.0.0: also allow localhost + all local network interfaces
        if host == "0.0.0.0" {
            origins.push("http://localhost:5173".parse().expect("valid CORS origin"));
            origins.push(format!("http://localhost:{port}").parse().expect("valid CORS origin"));
            // Add all non-loopback IPv4 addresses (for LAN access)
            for iface in get_local_ipv4_addrs() {
                if let Ok(v) = format!("http://{iface}:{port}").parse() { origins.push(v); }
                if let Ok(v) = format!("http://{iface}:5173").parse() { origins.push(v); }
            }
            // Add Docker subnet gateway IPs for CORS
            for gw in get_docker_subnet_gateways(&state.config.config.gateway.cors_docker_subnets) {
                if let Ok(v) = format!("http://{gw}:{port}").parse() { origins.push(v); }
                if let Ok(v) = format!("http://{gw}:5173").parse() { origins.push(v); }
            }
        }
        // Also add public_url origin if configured
        if let Some(ref pu) = state.config.config.gateway.public_url
            && let Ok(v) = pu.trim_end_matches('/').parse() { origins.push(v); }
        origins
    } else {
        state.config.config.gateway.cors_origins.iter()
            .filter_map(|o| o.parse().ok())
            .collect()
    };
    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin(cors_origins)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::ACCEPT,
        ]);
    // Serve static UI files from ui/out/ with SPA fallback to index.html.
    // API routes have priority (registered above); unmatched paths serve static files.
    // _next/ assets served WITHOUT fallback (404 if missing — prevents stale cache getting HTML).
    // All other paths fall back to index.html for SPA routing.
    let ui_dir = std::path::Path::new("ui/out");
    let app = if ui_dir.is_dir() {
        // _next/ assets are content-hashed → cache forever (immutable)
        let next_service = ServeDir::new(ui_dir.join("_next"));
        let app = app.nest_service(
            "/_next",
            tower_http::set_header::SetResponseHeader::overriding(
                next_service,
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        );
        // HTML/other files → always revalidate
        let serve = tower_http::set_header::SetResponseHeader::overriding(
            ServeDir::new(ui_dir).fallback(ServeFile::new(ui_dir.join("index.html"))),
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-cache"),
        );
        app.fallback_service(serve)
    } else {
        app
    };

    let app = app.layer(cors);

    // Security headers: prevent MIME sniffing, clickjacking, XSS reflection
    let app = app.layer(axum_mw::from_fn(|req: axum::http::Request<axum::body::Body>, next: axum_mw::Next| async move {
        let mut response = next.run(req).await;
        let headers = response.headers_mut();
        headers.insert("X-Content-Type-Options", "nosniff".parse().expect("valid header value"));
        headers.insert("X-Frame-Options", "DENY".parse().expect("valid header value"));
        headers.insert("X-XSS-Protection", "1; mode=block".parse().expect("valid header value"));
        headers.insert("Referrer-Policy", "strict-origin-when-cross-origin".parse().expect("valid header value"));
        // Content-Security-Policy: defense-in-depth for the static SPA. The app
        // bundles its own assets locally, so 'self' covers them; 'unsafe-inline'
        // + 'unsafe-eval' + 'wasm-unsafe-eval' are required by the Next.js
        // static export (inline hydration scripts) and WASM-backed libs (shiki).
        // The trusted CDNs (jsdelivr/unpkg/cdnjs) are allowed for script/style/
        // font because agent-generated canvas HTML renders inside a sandboxed
        // iframe (`sandbox="allow-scripts"`, no same-origin → isolated from the
        // parent) and legitimately pulls libs like mermaid from a CDN; srcdoc
        // iframes inherit this CSP, so the allowance must live here. The policy
        // still hard-locks object/base/form/frame-ancestors and keeps
        // connect-src first-party.
        headers.insert(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com; style-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com; img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self' data: https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'"
                .parse().expect("valid header value"),
        );
        // HSTS: the public origin (hc.aronmav.ru) is HTTPS-only behind nginx.
        // Browsers ignore this header over plain HTTP (LAN / loopback), so it is
        // safe to emit unconditionally. No includeSubDomains — scoped to the
        // serving host to avoid affecting sibling HTTP subdomains.
        headers.insert(
            "Strict-Transport-Security",
            "max-age=31536000".parse().expect("valid header value"),
        );
        response
    }));

    // Phase 65 OBS-04: W3C Trace Context middleware — OUTERMOST layer.
    //
    // Axum semantics: `.layer(A).layer(B)` means `B` wraps `A`, so `B` runs
    // FIRST on request ingress. By adding this layer LAST in the chain,
    // trace_context_middleware becomes the first middleware to see every
    // incoming request — before auth, before rate-limiting, before CORS.
    //
    // Consequence: even 401 / 403 / 429 responses carry a trace_id extension
    // for diagnostic correlation, satisfying the roadmap's "grep <trace_id>
    // in journalctl returns the full lifecycle of one request" goal.
    let app = app.layer(axum_mw::from_fn(trace_context::trace_context_middleware));

    // OTel-only: extract W3C parent context from incoming `traceparent` and
    // attach it to a fresh `http_request` span so downstream pipeline spans
    // (e.g. `pipeline.execute`) inherit the upstream trace_id. Complements
    // `trace_context_middleware` above (which only handles plain-text
    // logging correlation). Layered LAST → runs FIRST on ingress, before
    // any OTel-instrumented downstream span opens.
    let app = app.layer(axum_mw::from_fn(crate::trace_propagation::extract_trace_context_layer));

    Ok(app.with_state(state))
}

/// Get the primary non-loopback IPv4 address of the host (for CORS auto-derivation).
fn get_local_ipv4_addrs() -> Vec<String> {
    // UDP connect trick: connect to external IP (no actual traffic sent),
    // then read local_addr to get the outbound interface IP.
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0")
        && sock.connect("8.8.8.8:80").is_ok()
            && let Ok(local) = sock.local_addr()
                && !local.ip().is_loopback() {
                    return vec![local.ip().to_string()];
                }
    Vec::new()
}

/// Parse CIDR subnets and return their gateway IPs (.1 address).
/// e.g. "172.17.0.0/16" -> "172.17.0.1"
fn get_docker_subnet_gateways(subnets: &[String]) -> Vec<String> {
    subnets.iter().filter_map(|cidr| {
        let ip_part = cidr.split('/').next()?;
        let octets: Vec<&str> = ip_part.split('.').collect();
        if octets.len() == 4 {
            Some(format!("{}.{}.{}.1", octets[0], octets[1], octets[2]))
        } else {
            None
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::handlers::agents::{validate_agent_name, agent_config_path};
    use super::handlers::secrets::mask_secret_value;
    use super::handlers::workspace::format_workspace_size;
    use super::handlers::skills::skill_safe_name;

    // ── mask_secret_value ────────────────────────────────────────────────────

    #[test]
    fn mask_empty_string() {
        assert_eq!(mask_secret_value(""), "");
    }

    #[test]
    fn mask_short_3_chars() {
        assert_eq!(mask_secret_value("abc"), "***");
    }

    #[test]
    fn mask_exactly_8_chars() {
        assert_eq!(mask_secret_value("12345678"), "********");
    }

    #[test]
    fn mask_9_chars() {
        assert_eq!(mask_secret_value("123456789"), "1234...6789");
    }

    #[test]
    fn mask_12_chars() {
        assert_eq!(mask_secret_value("abcdefghijkl"), "abcd...ijkl");
    }

    // ── validate_agent_name ──────────────────────────────────────────────────

    #[test]
    fn validate_agent_name_valid_compound() {
        assert!(validate_agent_name("my-agent_1").is_ok());
    }

    #[test]
    fn validate_agent_name_single_char() {
        assert!(validate_agent_name("a").is_ok());
    }

    #[test]
    fn validate_agent_name_empty() {
        assert!(validate_agent_name("").is_err());
    }

    #[test]
    fn validate_agent_name_too_long() {
        let name = "a".repeat(33);
        assert!(validate_agent_name(&name).is_err());
    }

    #[test]
    fn validate_agent_name_special_chars() {
        assert!(validate_agent_name("my agent!").is_err());
    }

    #[test]
    fn validate_agent_name_dash_underscore() {
        assert!(validate_agent_name("my_agent-1").is_ok());
    }

    #[test]
    fn validate_agent_name_exactly_32_chars() {
        let name = "a".repeat(32);
        assert!(validate_agent_name(&name).is_ok());
    }

    // ── agent_config_path ────────────────────────────────────────────────────

    #[test]
    fn agent_config_path_main() {
        let path = agent_config_path("main");
        assert_eq!(path, std::path::Path::new("config/agents/main.toml"));
    }

    // ── format_workspace_size ────────────────────────────────────────────────

    #[test]
    fn format_workspace_size_zero() {
        assert_eq!(format_workspace_size(0), "0 B");
    }

    #[test]
    fn format_workspace_size_bytes() {
        assert_eq!(format_workspace_size(500), "500 B");
    }

    #[test]
    fn format_workspace_size_exactly_1_kb() {
        assert_eq!(format_workspace_size(1024), "1.0 KB");
    }

    #[test]
    fn format_workspace_size_1_5_kb() {
        assert_eq!(format_workspace_size(1536), "1.5 KB");
    }

    #[test]
    fn format_workspace_size_exactly_1_mb() {
        assert_eq!(format_workspace_size(1_048_576), "1.0 MB");
    }

    // ── skill_safe_name ──────────────────────────────────────────────────────

    #[test]
    fn skill_safe_name_unchanged() {
        assert_eq!(skill_safe_name("simple-name"), "simple-name");
    }

    #[test]
    fn skill_safe_name_slashes() {
        assert_eq!(skill_safe_name("path/to\\file"), "path-to-file");
    }

    #[test]
    fn skill_safe_name_spaces() {
        assert_eq!(skill_safe_name("name with spaces"), "name-with-spaces");
    }

    #[test]
    fn skill_safe_name_all_special_chars() {
        // : * ? " < > | all replaced with - and trailing/leading runs of -
        // are trimmed (audit 2026-05-08 path-traversal hardening).
        assert_eq!(
            skill_safe_name("file:name*bad?\"<>|"),
            "file-name-bad"
        );
    }

    #[test]
    fn skill_safe_name_collapses_dot_dot() {
        // Path-traversal regression guard (audit 2026-05-08): '..' must
        // collapse to '-' and the result must not start with '.' or be
        // empty.
        assert_eq!(skill_safe_name(".."), "_unnamed");
        assert_eq!(skill_safe_name("../etc/passwd"), "etc-passwd");
        assert_eq!(skill_safe_name("..."), "_unnamed");
    }

    // ── docker_subnet_gateways ──────────────────────────────────────────────

    #[test]
    fn docker_subnet_gateway_basic() {
        let subnets = vec!["172.17.0.0/16".to_string()];
        let gws = super::get_docker_subnet_gateways(&subnets);
        assert_eq!(gws, vec!["172.17.0.1"]);
    }

    #[test]
    fn docker_subnet_gateway_multiple() {
        let subnets = vec![
            "172.17.0.0/16".to_string(),
            "172.18.0.0/16".to_string(),
        ];
        let gws = super::get_docker_subnet_gateways(&subnets);
        assert_eq!(gws, vec!["172.17.0.1", "172.18.0.1"]);
    }

    #[test]
    fn docker_subnet_gateway_empty() {
        let gws = super::get_docker_subnet_gateways(&[]);
        assert!(gws.is_empty());
    }

    #[test]
    fn docker_subnet_gateway_invalid() {
        let subnets = vec!["not-a-cidr".to_string()];
        let gws = super::get_docker_subnet_gateways(&subnets);
        assert!(gws.is_empty());
    }
}
