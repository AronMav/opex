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

// ── URL validation (sync, no DNS) ────────────────────────────────────────────

/// Validate URL scheme, internal-service blocklist, and numeric private IPs.
///
/// This is a **sync** pre-check. DNS-based private-IP filtering happens at
/// connection time via [`SsrfSafeResolver`]. Use [`preflight_resolve`] if you
/// need to bail BEFORE spending a full connect timeout on a hostname whose
/// first DNS answer is private.
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
}
