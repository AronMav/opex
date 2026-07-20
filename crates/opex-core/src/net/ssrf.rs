//! Unified SSRF (Server-Side Request Forgery) guard.
//!
//! Phase 64 SEC-01 consolidates two previously parallel code paths (the old
//! `src/tools/ssrf.rs` + any raw `reqwest::Client` construction in webhook
//! outbound flow) into ONE canonical surface:
//!
//! * [`validate_url_scheme`] — sync pre-check on scheme + internal blocklist +
//!   numeric-IP private-range filter. Returns typed [`SsrfError`].
//! * [`preflight_resolve`] — async DNS preflight. Fails CLOSED on any private
//!   IP in the resolution set, on parse failures, and on DNS failures.
//! * [`SsrfSafeResolver`] — DNS-level filter. Plugged into every outbound
//!   `reqwest::Client` so EVERY resolution (not just a cached preflight)
//!   rejects private IPs. This closes the DNS-rebinding TOCTOU gap.
//! * [`ssrf_http_client`] — canonical client builder: 30s default timeout,
//!   10s connect timeout, redirect policy NONE, safe DNS resolver.
//!
//! Private-IP set enforced by [`is_private_ip`]:
//!
//! * IPv4: RFC 1918 (10/8, 172.16/12, 192.168/16), loopback (127/8),
//!   link-local (169.254/16), CGNAT (100.64/10), multicast (224/4),
//!   broadcast (255.255.255.255), unspecified (0.0.0.0).
//! * IPv6: loopback (::1), unspecified (::), ULA (fc00::/7), link-local
//!   (fe80::/10), Teredo (2001::/32), 6to4 (2002::/16), multicast (ff00::/8),
//!   IPv4-mapped (::ffff:x.x.x.x — recurses on the embedded v4).

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use thiserror::Error;

/// Structured errors returned by the SSRF guard.
///
/// Variants map to stable HTTP status codes at the gateway layer:
///
/// * [`SsrfError::InvalidUrl`] → `400 Bad Request`
/// * [`SsrfError::BlockedScheme`] → `400 Bad Request`
/// * [`SsrfError::InternalBlocked`] → `400 Bad Request` with the literal
///   body `{"error":"target resolves to private IP"}` (see CONTEXT.md).
/// * [`SsrfError::PrivateIpResolved`] → `400 Bad Request` with the same body.
#[derive(Debug, Error)]
pub enum SsrfError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("blocked scheme: {0}")]
    BlockedScheme(String),

    #[error("blocked: URL targets internal service ({0})")]
    InternalBlocked(String),

    #[error("target resolves to private IP")]
    PrivateIpResolved,
}

/// Cloud-metadata hostnames blocked from user-supplied URLs at the sync
/// pre-check layer (Batch K, gap #4). These DNS names all resolve to the
/// well-known link-local instance-metadata address (169.254.169.254 on
/// AWS/GCP/Azure/DigitalOcean) which IS already rejected by
/// [`preflight_resolve`] and [`SsrfSafeResolver`] at DNS-resolution time —
/// but `validate_url_scheme` itself is a **sync, no-DNS** pre-check, so a
/// caller that uses it as its sole guard (without routing the actual request
/// through `ssrf_http_client`/`preflight_resolve`) would previously let these
/// hostnames slip past this layer. Matched case-insensitively, exact-or-suffix
/// against the URL host (so `metadata.google.internal.` / subdomains of
/// `metadata.goog` are also caught).
const METADATA_HOSTNAME_BLOCKLIST: &[&str] = &[
    "metadata.google.internal",
    "metadata.goog",
    "metadata",
];

/// True if `host` is (or is a subdomain of) a known cloud-metadata hostname.
fn is_blocked_metadata_hostname(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    METADATA_HOSTNAME_BLOCKLIST
        .iter()
        .any(|blocked| host == *blocked || host.ends_with(&format!(".{blocked}")))
}

/// Internal services blocked from user-supplied URLs (reachable only via
/// service-to-service calls originating from trusted code paths).
const INTERNAL_BLOCKLIST: &[&str] = &[
    "localhost:9011",
    "toolgate:9011",
    "localhost:9020",
    "browser-renderer:9020",
    "localhost:8080",
    "searxng:8080",
    "localhost:18789",
    "localhost:5432", // PostgreSQL
    "postgres:5432",
    "localhost:2375", // Docker API (prevent SSRF to Docker socket)
];

/// Check if an IP address belongs to any blocked / private / link-local /
/// CGNAT / multicast / broadcast / tunnel-brokerage range.
///
/// Visibility is `pub(crate)` so the internal `SsrfSafeResolver` and preflight
/// paths share one source of truth; external callers should use
/// [`validate_url_scheme`] or [`preflight_resolve`] instead.
pub(crate) fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()                 // 127.0.0.0/8
                || v4.is_private()           // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local()        // 169.254.0.0/16
                || v4.is_broadcast()         // 255.255.255.255
                || v4.is_multicast()         // 224.0.0.0/4 (Phase 64 CONTEXT)
                || v4 == std::net::Ipv4Addr::UNSPECIFIED // 0.0.0.0
                // 100.64.0.0/10 — Carrier-grade NAT
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64)
        }
        IpAddr::V6(v6) => {
            // Recurse into IPv4-mapped form so ::ffff:127.0.0.1 maps to loopback.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            v6.is_loopback()                 // ::1
                || v6 == Ipv6Addr::UNSPECIFIED // ::
                || v6.is_multicast()         // ff00::/8 (Phase 64 CONTEXT)
                // RFC 4193 Unique Local fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // Teredo tunnel 2001::/32 (Phase 64 CONTEXT) — the full prefix
                // is 2001:0000::/32 so both the first and second u16 must be 0.
                || (v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0000)
                // 6to4 2002::/16 (Phase 64 CONTEXT)
                || v6.segments()[0] == 0x2002
        }
    }
}

/// IPs that even a `allow_private_endpoint` ("LAN") tool must NEVER reach:
/// loopback, link-local (incl. `169.254.169.254` cloud metadata), CGNAT,
/// unspecified, and multicast/broadcast — plus the IPv6 equivalents. Unlike
/// [`is_private_ip`], the RFC1918 private ranges (10/8, 172.16/12, 192.168/16)
/// are PERMITTED here: that is the whole point of the LAN client — reach a
/// home-lab service over a trusted LAN/tunnel while still blocking the ranges
/// an SSRF attack actually wants (metadata, Docker, loopback infra).
pub(crate) fn is_dangerous_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local() // 169.254/16 incl. cloud metadata
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4 == std::net::Ipv4Addr::UNSPECIFIED
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64) // CGNAT
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_dangerous_ip(IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6 == Ipv6Addr::UNSPECIFIED
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// DNS resolver for the LAN client: filters only [`is_dangerous_ip`] answers,
/// permitting RFC1918 private targets. Same DNS-rebinding-safe design as
/// [`SsrfSafeResolver`] (filters on every resolve, not a cached preflight).
pub struct SsrfLanResolver;

impl reqwest::dns::Resolve for SsrfLanResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = format!("{}:0", name.as_str());
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&host)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .filter(|a| !is_dangerous_ip(a.ip()))
                .collect();

            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "SSRF blocked: '{}' resolves only to dangerous (loopback/metadata/CGNAT) IPs",
                        name.as_str()
                    ),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }

            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Like [`ssrf_http_client`] but permits RFC1918 private LAN targets (still
/// blocks loopback, cloud-metadata/link-local, and CGNAT via [`SsrfLanResolver`]).
/// For admin-authored YAML tools that set `allow_private_endpoint: true` to reach
/// a home-lab / tunnel service. Redirect policy is `none` (same as the SSRF client).
pub fn lan_http_client(timeout: std::time::Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(std::time::Duration::from_secs(10))
        .dns_resolver(Arc::new(SsrfLanResolver))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build LAN HTTP client")
}

// ── SSRF-safe DNS resolver ───────────────────────────────────────────────────

/// Custom DNS resolver that filters out every private / internal IP answer.
///
/// When plugged into [`reqwest::ClientBuilder::dns_resolver`] every TCP
/// connection uses only public IPs, eliminating the DNS-rebinding TOCTOU gap
/// where a hostname could resolve to a public IP during validation but a
/// private IP during connect. Plan 01's `DnsRebindingResolver` fixture proves
/// the filter applies on EVERY resolve call, not just a cached preflight.
pub struct SsrfSafeResolver;

impl reqwest::dns::Resolve for SsrfSafeResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = format!("{}:0", name.as_str());
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&host)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .filter(|a| !is_private_ip(a.ip()))
                .collect();

            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "SSRF blocked: '{}' resolves only to private/internal IPs",
                        name.as_str()
                    ),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }

            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Returns `true` if the URL targets a known internal service (toolgate,
/// searxng, browser-renderer, PostgreSQL). Internal service-to-service calls
/// originating from trusted code paths bypass SSRF filtering — the callers
/// explicitly opt in by checking this predicate.
pub fn is_internal_endpoint(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or("");
    let port = parsed.port_or_known_default().unwrap_or(80);
    let authority = format!("{host}:{port}");
    INTERNAL_BLOCKLIST.iter().any(|a| *a == authority)
}

/// Build a reqwest client configured with the SSRF-safe DNS resolver and the
/// canonical redirect=none policy. This is the ONE builder that every
/// user-URL outbound HTTP client in the codebase should use.
pub fn ssrf_http_client(timeout: std::time::Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(std::time::Duration::from_secs(10))
        .dns_resolver(Arc::new(SsrfSafeResolver))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build SSRF-safe HTTP client")
}

/// Select the correct outbound HTTP client for a request whose target is a
/// (possibly admin-authored YAML tool) `endpoint`, with a caller-provided
/// timeout for slow generators (TTS / imagegen background jobs).
///
/// Mirrors the client-selection already used by the regular YAML-tool
/// dispatch paths (`agent/engine_dispatch.rs`, `agent/pipeline/handlers.rs`
/// `handle_tool_test`): admin-configured internal services (toolgate,
/// browser-renderer, …) recognised by [`is_internal_endpoint`] use the plain
/// client (no DNS filter needed — the target is trusted and fixed), every
/// other endpoint gets a fresh SSRF-safe client (private-IP DNS filter +
/// `redirect(Policy::none())`) built with the same `timeout`.
///
/// This closes the `channel_action` (TTS/imagegen) bypass: those code paths
/// used to build a raw `reqwest::Client::builder()` with no SSRF protection
/// at all, regardless of endpoint. See docs/superpowers/plans/triage/T01-ssrf-redirect.md §3.
pub fn select_ssrf_aware_client(endpoint: &str, timeout: std::time::Duration) -> reqwest::Client {
    if is_internal_endpoint(endpoint) {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    } else {
        ssrf_http_client(timeout)
    }
}

/// Validate an outbound endpoint destined for the [`select_ssrf_aware_client`]
/// path. Trusted internal services (toolgate, browser-renderer, …) pass
/// unchecked; every other endpoint must clear [`validate_url_scheme`].
///
/// This is the companion sync pre-check that closes the *literal-IP* hole in
/// `ssrf_http_client`: reqwest connects straight to a literal IP written into
/// the URL (`http://169.254.169.254/…`) without ever invoking the
/// [`SsrfSafeResolver`], so the DNS filter alone never sees it.
/// `validate_url_scheme` rejects literal private/metadata IPs (and bad
/// schemes) inline. Callers that build an SSRF-aware client for a
/// possibly-hostile endpoint must gate the request on this first.
pub fn validate_outbound_endpoint(endpoint: &str) -> Result<(), SsrfError> {
    if is_internal_endpoint(endpoint) {
        return Ok(());
    }
    validate_url_scheme(endpoint)
}

/// Literal-IP sync pre-check for the `allow_private_endpoint` LAN path
/// ([`lan_http_client`] / [`SsrfLanResolver`]). Permits RFC1918 private targets
/// (the whole point of the LAN client) but rejects a literal loopback /
/// cloud-metadata / link-local / CGNAT IP written directly into the URL — the
/// same literal-IP hole [`validate_outbound_endpoint`] closes for the default
/// path, since reqwest bypasses [`SsrfLanResolver`] for literal IPs. Hostnames
/// still resolve through the DNS-filtering [`SsrfLanResolver`] at connect time.
pub fn validate_lan_endpoint(endpoint: &str) -> Result<(), SsrfError> {
    let parsed = reqwest::Url::parse(endpoint)
        .map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(SsrfError::BlockedScheme(scheme.to_string())),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| SsrfError::InvalidUrl("URL has no host".to_string()))?;
    // Only literal IPs bypass the resolver; hostnames are filtered at connect.
    if let Ok(ip) = host.parse::<IpAddr>()
        && is_dangerous_ip(ip)
    {
        return Err(SsrfError::InternalBlocked(host.to_string()));
    }
    Ok(())
}

// ── URL validation (sync, no DNS) ────────────────────────────────────────────

/// Validate URL scheme, internal-service blocklist, cloud-metadata hostnames,
/// and numeric private IPs.
///
/// This is a **sync** pre-check. DNS-based private-IP filtering happens at
/// connection time via [`SsrfSafeResolver`]. Use [`preflight_resolve`] if you
/// need to bail BEFORE spending a full connect timeout on a hostname whose
/// first DNS answer is private. Cloud-metadata hostnames
/// (`metadata.google.internal`, `metadata.goog`, `metadata`) are rejected
/// here directly (see [`METADATA_HOSTNAME_BLOCKLIST`]) so a caller relying
/// solely on this sync check — without also going through a DNS-aware path —
/// is still covered.
pub fn validate_url_scheme(url: &str) -> Result<(), SsrfError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;

    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(SsrfError::BlockedScheme(scheme.to_string())),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| SsrfError::InvalidUrl("URL has no host".to_string()))?;

    let port = parsed.port_or_known_default().unwrap_or(80);
    let authority = format!("{host}:{port}");

    if INTERNAL_BLOCKLIST.iter().any(|a| *a == authority) {
        return Err(SsrfError::InternalBlocked(authority));
    }

    // Cloud-metadata DNS names (Batch K, gap #4) — reject at the sync
    // pre-check layer too, not just at DNS-resolution time.
    if is_blocked_metadata_hostname(host) {
        return Err(SsrfError::InternalBlocked(format!(
            "URL targets cloud-metadata hostname ({host})"
        )));
    }

    // Numeric IP in URL — bypasses DNS so we must check inline.
    // Use parsed.host() for bracketed IPv6 support ([::ffff:127.0.0.1]).
    let ip: Option<IpAddr> = match parsed.host() {
        Some(url::Host::Ipv4(v4)) => Some(IpAddr::V4(v4)),
        Some(url::Host::Ipv6(v6)) => Some(IpAddr::V6(v6)),
        _ => None,
    };
    if let Some(ip) = ip
        && is_private_ip(ip)
    {
        return Err(SsrfError::InternalBlocked(format!(
            "URL targets private IP address ({host})"
        )));
    }

    Ok(())
}

/// Async DNS preflight.
///
/// Fails CLOSED when:
///
/// * The input parses as a literal IP and the IP is private — returns
///   [`SsrfError::PrivateIpResolved`].
/// * DNS lookup succeeds and ANY returned address is private — returns
///   [`SsrfError::PrivateIpResolved`].
/// * DNS lookup fails outright (NXDOMAIN, timeout) — returns
///   [`SsrfError::PrivateIpResolved`]. Rationale: a failed lookup is
///   unverifiable, so we can't prove the target is public; refuse.
///
/// This is the companion to [`SsrfSafeResolver`]: the resolver enforces at
/// every subsequent connect, but the preflight gives callers an early, typed
/// rejection for observability and for the gateway to return a structured
/// 400 before burning a full connect timeout.
pub async fn preflight_resolve(host: &str) -> Result<(), SsrfError> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_private_ip(ip) {
            Err(SsrfError::PrivateIpResolved)
        } else {
            Ok(())
        };
    }

    let lookup_target = format!("{host}:0");
    match tokio::net::lookup_host(&lookup_target).await {
        Ok(addrs) => {
            let mut saw_any = false;
            for a in addrs {
                saw_any = true;
                if is_private_ip(a.ip()) {
                    return Err(SsrfError::PrivateIpResolved);
                }
            }
            if saw_any {
                Ok(())
            } else {
                // No A/AAAA records — fail closed.
                Err(SsrfError::PrivateIpResolved)
            }
        }
        Err(_) => Err(SsrfError::PrivateIpResolved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn private_ipv4_ranges() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("169.254.1.1".parse().unwrap()));
        assert!(is_private_ip("0.0.0.0".parse().unwrap()));
        assert!(is_private_ip("100.64.0.1".parse().unwrap())); // CGNAT
        assert!(is_private_ip("224.0.0.1".parse().unwrap())); // multicast (Phase 64)
        assert!(is_private_ip("239.255.255.255".parse().unwrap())); // multicast (Phase 64)
        assert!(is_private_ip("255.255.255.255".parse().unwrap())); // broadcast

        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_private_ip("93.184.216.34".parse().unwrap()));
        assert!(!is_private_ip("100.128.0.1".parse().unwrap())); // outside CGNAT
    }

    #[test]
    fn private_ipv6_ranges() {
        assert!(is_private_ip("::1".parse().unwrap()));
        assert!(is_private_ip("fc00::1".parse().unwrap()));
        assert!(is_private_ip("fd00::1".parse().unwrap()));
        assert!(is_private_ip("fe80::1".parse().unwrap()));
        // Phase 64 CONTEXT additions
        assert!(is_private_ip("2001::1".parse().unwrap())); // Teredo
        assert!(is_private_ip("2002::1".parse().unwrap())); // 6to4
        assert!(is_private_ip("ff00::1".parse().unwrap())); // multicast
        assert!(is_private_ip("ff02::1".parse().unwrap())); // multicast link-local
        // IPv4-mapped
        assert!(is_private_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:100.64.0.1".parse().unwrap()));

        // Public IPv6 should pass
        assert!(!is_private_ip("2606:4700:4700::1111".parse().unwrap()));
        assert!(!is_private_ip("::ffff:8.8.8.8".parse().unwrap()));
        // 2001:4860::/32 is Google DNS — NOT Teredo. Teredo is exactly 2001:0000::/32.
        // The second 16-bit word must be zero to hit the Teredo gate.
        assert!(!is_private_ip("2001:4860:4860::8888".parse().unwrap()));
    }

    #[test]
    fn teredo_prefix_strict() {
        // Only 2001:0000::/32 should hit — other 2001:xxxx:: blocks are public.
        let teredo = IpAddr::V6("2001:0000:abcd::1".parse::<Ipv6Addr>().unwrap());
        assert!(is_private_ip(teredo));
        let not_teredo = IpAddr::V6("2001:4860:4860::8888".parse::<Ipv6Addr>().unwrap());
        assert!(!is_private_ip(not_teredo));
    }

    #[test]
    fn blocked_schemes_rejected() {
        assert!(matches!(
            validate_url_scheme("file:///etc/passwd"),
            Err(SsrfError::BlockedScheme(_))
        ));
        assert!(matches!(
            validate_url_scheme("ftp://evil.com/file"),
            Err(SsrfError::BlockedScheme(_))
        ));
        assert!(matches!(
            validate_url_scheme("gopher://evil.com"),
            Err(SsrfError::BlockedScheme(_))
        ));
    }

    #[test]
    fn internal_services_blocked() {
        assert!(matches!(
            validate_url_scheme("http://localhost:9011/api"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://toolgate:9011/describe-url"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://localhost:18789/api/secrets"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://localhost:5432"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://postgres:5432"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://localhost:2375"),
            Err(SsrfError::InternalBlocked(_))
        ));
    }

    #[test]
    fn public_urls_allowed() {
        assert!(validate_url_scheme("https://example.com").is_ok());
        assert!(validate_url_scheme("http://api.github.com/repos").is_ok());
        assert!(validate_url_scheme("http://8.8.8.8:80").is_ok());
    }

    #[test]
    fn unparseable_urls_rejected() {
        assert!(matches!(
            validate_url_scheme("not-a-url"),
            Err(SsrfError::InvalidUrl(_))
        ));
        assert!(matches!(
            validate_url_scheme("://missing-scheme"),
            Err(SsrfError::InvalidUrl(_))
        ));
        assert!(matches!(
            validate_url_scheme(""),
            Err(SsrfError::InvalidUrl(_))
        ));
    }

    #[test]
    fn numeric_private_ips_blocked() {
        assert!(matches!(
            validate_url_scheme("http://127.0.0.1:2375"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://10.0.0.1:80"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://192.168.1.1:8080"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://224.0.0.1"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://255.255.255.255"),
            Err(SsrfError::InternalBlocked(_))
        ));
    }

    #[test]
    fn numeric_ipv6_private_blocked() {
        assert!(matches!(
            validate_url_scheme("http://[::ffff:127.0.0.1]:8080"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://[::ffff:10.0.0.1]"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://[::1]:8080"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://[2001::1]"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://[2002::1]"),
            Err(SsrfError::InternalBlocked(_))
        ));
        // Public IPv6 should be allowed
        assert!(validate_url_scheme("http://[2606:4700:4700::1111]").is_ok());
    }

    #[test]
    fn default_port_mapping() {
        assert!(validate_url_scheme("http://localhost:80/api").is_ok());
        assert!(matches!(
            validate_url_scheme("http://localhost:9011"),
            Err(SsrfError::InternalBlocked(_))
        ));
    }

    #[test]
    fn is_internal_endpoint_cases() {
        assert!(is_internal_endpoint("http://localhost:9011/generate-image"));
        assert!(is_internal_endpoint("http://localhost:9011/describe-url"));
        assert!(is_internal_endpoint("http://localhost:8080/search"));
        assert!(is_internal_endpoint("http://browser-renderer:9020/automation"));
        assert!(is_internal_endpoint("http://searxng:8080/search"));

        assert!(!is_internal_endpoint("https://api.fal.ai/generate"));
        assert!(!is_internal_endpoint("https://api.openai.com/v1/chat"));
        assert!(!is_internal_endpoint("http://example.com:9011/test"));
    }

    #[test]
    fn lan_client_permits_rfc1918_but_blocks_dangerous() {
        // The LAN client (allow_private_endpoint) must reach home-lab/tunnel
        // RFC1918 targets while STILL blocking the ranges an SSRF attack wants.
        for allowed in ["192.168.1.10", "10.8.0.2", "172.16.5.4"] {
            assert!(
                !is_dangerous_ip(allowed.parse::<IpAddr>().unwrap()),
                "{allowed} should be permitted by the LAN client"
            );
        }
        for blocked in [
            "127.0.0.1",       // loopback
            "169.254.169.254", // cloud metadata (link-local)
            "100.64.1.1",      // CGNAT
            "0.0.0.0",         // unspecified
        ] {
            assert!(
                is_dangerous_ip(blocked.parse::<IpAddr>().unwrap()),
                "{blocked} must stay blocked even for the LAN client"
            );
        }
        // Full SSRF client still blocks RFC1918 (unchanged behaviour).
        assert!(is_private_ip("192.168.1.10".parse::<IpAddr>().unwrap()));
    }

    #[tokio::test]
    async fn preflight_literal_public_ip_ok() {
        assert!(preflight_resolve("8.8.8.8").await.is_ok());
        assert!(preflight_resolve("1.1.1.1").await.is_ok());
    }

    #[tokio::test]
    async fn preflight_literal_private_ip_rejected() {
        assert!(matches!(
            preflight_resolve("127.0.0.1").await,
            Err(SsrfError::PrivateIpResolved)
        ));
        assert!(matches!(
            preflight_resolve("10.0.0.1").await,
            Err(SsrfError::PrivateIpResolved)
        ));
        assert!(matches!(
            preflight_resolve("224.0.0.1").await,
            Err(SsrfError::PrivateIpResolved)
        ));
    }

    #[tokio::test]
    async fn preflight_dns_failure_fails_closed() {
        // A clearly non-resolvable hostname — must fail closed.
        // Use a .invalid TLD (RFC 2606 reserved-for-test) so we don't
        // accidentally hit a real resolver success path.
        let result =
            preflight_resolve("absolutely-does-not-exist.invalid").await;
        assert!(matches!(result, Err(SsrfError::PrivateIpResolved)));
    }

    #[test]
    fn ipv4_unspecified_flagged() {
        // 0.0.0.0 routes to local interface — must be blocked.
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
    }

    // ── H5a (T08 pt.5): explicit cloud-metadata endpoint coverage ──────────

    #[test]
    fn validate_url_scheme_blocks_numeric_cloud_metadata_ip() {
        // 169.254.169.254 is the well-known AWS/GCP/Azure/DigitalOcean cloud
        // instance-metadata endpoint. It falls under the 169.254.0.0/16
        // link-local range already covered by `is_private_ip`, but this test
        // pins the EXACT address explicitly at the `validate_url_scheme`
        // sync pre-check layer (not just the DNS-resolver layer exercised by
        // `select_client_non_internal_endpoint_blocks_private_ip`), so a
        // regression in either `is_private_ip`'s link-local branch or the
        // numeric-IP fast path in `validate_url_scheme` fails loudly here.
        assert!(matches!(
            validate_url_scheme("http://169.254.169.254/latest/meta-data/"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://169.254.169.254/"),
            Err(SsrfError::InternalBlocked(_))
        ));
        // Azure IMDS uses the same well-known address on a different path.
        assert!(matches!(
            validate_url_scheme("http://169.254.169.254/metadata/instance"),
            Err(SsrfError::InternalBlocked(_))
        ));
    }

    #[tokio::test]
    async fn preflight_resolve_blocks_numeric_cloud_metadata_ip() {
        assert!(matches!(
            preflight_resolve("169.254.169.254").await,
            Err(SsrfError::PrivateIpResolved)
        ));
    }

    // RESOLVED (Batch K, gap #4): `metadata.google.internal` is a DNS NAME,
    // not a literal IP. Previously `validate_url_scheme` (the sync, no-DNS
    // pre-check) could only reject it via the static `INTERNAL_BLOCKLIST`
    // (host:port strings), which did not include cloud-metadata hostnames —
    // those were only caught one layer down, at `preflight_resolve` (async
    // DNS lookup) / `SsrfSafeResolver` (actual connect time), because the
    // resolved answer is link-local. A caller using `validate_url_scheme` as
    // its sole guard (without routing the request through one of the
    // DNS-aware paths) would have had a narrow gap. `validate_url_scheme` now
    // also checks the URL hostname against `METADATA_HOSTNAME_BLOCKLIST`
    // directly, closing that gap without requiring DNS.
    #[test]
    fn metadata_google_internal_resolves_to_blocked_link_local_address() {
        // Pin the *documented* behavior: GCP's metadata server documents
        // 169.254.169.254 as the canonical address behind the
        // `metadata.google.internal` hostname — same address as AWS/Azure/DO.
        // Any DNS answer for that hostname is therefore covered by the
        // link-local branch of `is_private_ip`, which both `preflight_resolve`
        // and `SsrfSafeResolver` consult on every real resolution.
        assert!(is_private_ip("169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn validate_url_scheme_blocks_metadata_hostnames_at_sync_precheck() {
        // Batch K gap #4: these DNS names must now be rejected by the sync
        // pre-check itself, not only at DNS-resolution time.
        assert!(matches!(
            validate_url_scheme("http://metadata.google.internal/computeMetadata/v1/"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://metadata.goog/computeMetadata/v1/"),
            Err(SsrfError::InternalBlocked(_))
        ));
        assert!(matches!(
            validate_url_scheme("http://metadata/latest/meta-data/"),
            Err(SsrfError::InternalBlocked(_))
        ));
        // Case-insensitive.
        assert!(matches!(
            validate_url_scheme("http://METADATA.GOOGLE.INTERNAL/"),
            Err(SsrfError::InternalBlocked(_))
        ));
        // Subdomain form.
        assert!(matches!(
            validate_url_scheme("http://foo.metadata.google.internal/"),
            Err(SsrfError::InternalBlocked(_))
        ));
        // Legitimate hosts must not be caught by a substring match.
        assert!(validate_url_scheme("https://example.com/").is_ok());
        assert!(validate_url_scheme("https://mymetadata.example.com/").is_ok());
        // Existing numeric-IP regression must still pass.
        assert!(matches!(
            validate_url_scheme("http://169.254.169.254/"),
            Err(SsrfError::InternalBlocked(_))
        ));
    }

    // ── select_ssrf_aware_client (channel_action SSRF fix) ──────────────────

    #[test]
    fn select_client_internal_endpoint_uses_plain_builder() {
        // Internal endpoints (toolgate, browser-renderer, ...) are trusted —
        // no DNS filter, no redirect(Policy::none()) — but they DO get the
        // caller-provided long timeout for slow media generation.
        let client = select_ssrf_aware_client(
            "http://localhost:9011/v1/audio/speech",
            std::time::Duration::from_secs(600),
        );
        // We can't introspect timeout/redirect policy from a built `Client`
        // directly, so this test asserts the branch selection indirectly via
        // `is_internal_endpoint` (already unit-tested above) and that client
        // construction succeeds. The behavioural difference (DNS resolver +
        // redirect policy) is covered by the private-IP rejection test below.
        drop(client);
    }

    #[test]
    fn validate_outbound_endpoint_blocks_literal_private_and_metadata_ips() {
        // A channel_action YAML tool endpoint pointed at a *literal* private /
        // cloud-metadata IP must be rejected by the sync pre-check: reqwest
        // connects to a literal IP without ever calling the DNS resolver, so
        // `ssrf_http_client`'s resolver alone can't see it.
        //
        // Deterministic — NO real network I/O. (The previous version did a
        // real `.send()` to 169.254.169.254 and asserted failure; on cloud CI
        // runners that address is a live, reachable metadata endpoint, so the
        // request SUCCEEDED and the test flaked. That flake is exactly the
        // real bug this guard now closes.)
        for ep in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/admin",
            "http://192.168.1.1/",
            "http://127.0.0.1/",
            "http://metadata.google.internal/computeMetadata/v1/",
        ] {
            assert!(
                validate_outbound_endpoint(ep).is_err(),
                "expected {ep} to be blocked as a private/internal target",
            );
        }
    }

    #[test]
    fn validate_outbound_endpoint_allows_internal_and_public_hosts() {
        // Trusted internal services pass (routed to the plain client elsewhere).
        assert!(validate_outbound_endpoint("http://localhost:9011/v1/audio/speech").is_ok());
        // A public hostname passes the sync pre-check (no DNS here); the
        // SSRF-safe client still filters the resolved answer at connect time.
        assert!(validate_outbound_endpoint("https://api.openai.com/v1/audio/speech").is_ok());
    }

    #[test]
    fn validate_lan_endpoint_blocks_literal_metadata_and_loopback() {
        // F008/F009: the allow_private_endpoint path permits RFC1918 LAN IPs
        // but must still reject a literal loopback / cloud-metadata / CGNAT IP
        // written directly into the URL (reqwest bypasses SsrfLanResolver for
        // literal IPs).
        for ep in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1/admin",
            "http://100.64.0.1/", // CGNAT
        ] {
            assert!(
                validate_lan_endpoint(ep).is_err(),
                "expected LAN gate to block dangerous literal IP {ep}",
            );
        }
    }

    #[test]
    fn validate_lan_endpoint_permits_rfc1918_and_hostnames() {
        // The whole point of the LAN client: reach a home-lab service over a
        // private LAN. RFC1918 literals and hostnames pass the sync pre-check.
        assert!(validate_lan_endpoint("http://192.168.1.10/api").is_ok());
        assert!(validate_lan_endpoint("http://10.0.0.5:8080/").is_ok());
        assert!(validate_lan_endpoint("https://home.example.com/svc").is_ok());
    }

    #[test]
    fn validate_lan_endpoint_rejects_bad_scheme() {
        assert!(validate_lan_endpoint("file:///etc/passwd").is_err());
        assert!(validate_lan_endpoint("gopher://10.0.0.1/").is_err());
    }

    #[test]
    fn select_client_is_internal_endpoint_matches_dispatch_gate() {
        // Sanity: the predicate this helper relies on already recognises the
        // admin-configured internal services used by TTS/imagegen channel_action
        // tools (toolgate on 9011, browser-renderer on 9020).
        assert!(is_internal_endpoint("http://localhost:9011/v1/audio/speech"));
        assert!(is_internal_endpoint("http://browser-renderer:9020/automation"));
        assert!(!is_internal_endpoint("https://api.fal.ai/generate-image"));
    }
}
