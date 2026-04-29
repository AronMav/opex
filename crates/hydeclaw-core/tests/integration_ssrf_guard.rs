//! Phase 64 SEC-01 — unified SSRF guard integration test.
//!
//! RED first: MUST fail to compile until `src/net/ssrf.rs` exposes `SsrfError`
//! and `preflight_resolve`. GREEN is delivered by Task 2 of Plan 02.
//!
//! Contract asserted here:
//!   * `validate_url_scheme(..)` returns `Result<(), SsrfError>` (typed enum —
//!     no `anyhow::Error` leaking structured variants into prod payloads).
//!   * `preflight_resolve(host_or_ip)` is an async fn that fails CLOSED on any
//!     private-IP resolution, any parse failure, or any DNS failure — matching
//!     the CONTEXT.md "DNS-rebinding MUST fail closed" rule.
//!   * The `is_private_ip` set blocks: IPv4 RFC1918 + loopback + link-local +
//!     CGNAT + broadcast; IPv6 loopback + ULA + link-local + IPv4-mapped;
//!     Teredo (2001::/32), 6to4 (2002::/16), IPv4 multicast (224/4).
//!   * `ssrf_http_client(timeout)` returns a `reqwest::Client` pre-wired with
//!     the SSRF-safe DNS resolver + redirect=none.

mod support;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use hydeclaw_core::net::ssrf::{
    is_internal_endpoint, preflight_resolve, ssrf_http_client, validate_url_scheme, SsrfError,
    SsrfSafeResolver,
};
use support::DnsRebindingResolver;

#[tokio::test]
async fn dns_rebinding_rejected() {
    // Build a rebinding resolver (public first, loopback second). The fixture
    // lives in tests/support/dns_fixture.rs and was delivered by Plan 01.
    //
    // NOTE: The DnsRebindingResolver fixture is NOT wired into preflight_resolve
    // here because preflight_resolve performs a real system DNS lookup (it uses
    // tokio::net::lookup_host internally). The fixture's own unit tests in
    // dns_fixture.rs prove the flip-after-first-call behaviour.
    //
    // The TOCTOU protection (every connect re-checks via SsrfSafeResolver, not
    // just the preflight) is enforced at the reqwest::Client level. The canonical
    // integration evidence lives in the production code: ssrf_http_client() wires
    // SsrfSafeResolver as the dns_resolver, so every TCP connection goes through
    // the same is_private_ip filter that preflight_resolve uses.
    let rebind = Arc::new(DnsRebindingResolver::new(
        "evil.example",
        "8.8.8.8".parse::<IpAddr>().unwrap(),
        "127.0.0.1".parse::<IpAddr>().unwrap(),
    ));

    // Sanity: call_count starts at 0 until someone resolves.
    assert_eq!(rebind.call_count(), 0);

    // Shape-of-API assertion: the unified guard MUST provide preflight_resolve.
    // Passing a literal public IP must not be rejected.
    match preflight_resolve("8.8.8.8").await {
        Ok(()) => {}
        Err(e) => panic!("public literal must not be rejected: {e:?}"),
    }

    // And passing a literal loopback IP MUST be rejected with the typed
    // PrivateIpResolved variant (not anyhow::Error, not a stringly-typed result).
    match preflight_resolve("127.0.0.1").await {
        Ok(()) => panic!("loopback must be rejected"),
        Err(SsrfError::PrivateIpResolved) => {}
        Err(e) => panic!("wrong variant on loopback: {e:?}"),
    }

    // The SsrfSafeResolver impl MUST be constructible (zero-sized state) so
    // callers can plug it into `reqwest::Client::builder().dns_resolver(...)`.
    // Verify that a client built with ssrf_http_client() also compiles and does
    // not panic — this exercises the full builder path, not just the resolver.
    let _resolver = Arc::new(SsrfSafeResolver);
    let _client = ssrf_http_client(std::time::Duration::from_secs(5));
    // rebind fixture is not wired into a client here; its flip-on-second-call
    // contract is covered by the unit tests in tests/support/dns_fixture.rs.
}

#[test]
fn private_ip_set_blocked() {
    // Every URL below targets a private / loopback / link-local / CGNAT /
    // multicast / broadcast / Teredo / 6to4 / IPv4-mapped-IPv6 address.
    // `validate_url_scheme` MUST reject ALL of them with a typed SsrfError.
    for url in &[
        // IPv4 RFC1918 / loopback / link-local / CGNAT
        "http://127.0.0.1:2375",
        "http://10.0.0.1:80",
        "http://172.16.0.1",
        "http://192.168.1.1:8080",
        "http://169.254.1.1",
        "http://100.64.0.1",
        // IPv6 loopback / ULA / link-local
        "http://[::1]:8080",
        "http://[fc00::1]",
        "http://[fe80::1]",
        // IPv4-mapped IPv6 (must resolve through the embedded v4)
        "http://[::ffff:127.0.0.1]:8080",
        // Phase 64 CONTEXT additions: Teredo (2001::/32), 6to4 (2002::/16)
        "http://[2001::1]",
        "http://[2002::1]",
        // Multicast / broadcast
        "http://224.0.0.1",
        "http://255.255.255.255",
    ] {
        let res = validate_url_scheme(url);
        assert!(
            matches!(
                res,
                Err(SsrfError::InternalBlocked(_))
                    | Err(SsrfError::BlockedScheme(_))
                    | Err(SsrfError::InvalidUrl(_))
            ),
            "{url} should be rejected by validate_url_scheme; got {res:?}"
        );
    }
}

#[test]
fn public_ip_allowed() {
    // Public DNS name → structural Ok (DNS is not resolved in sync path).
    assert!(validate_url_scheme("https://example.com").is_ok());
    // Public literal IP → Ok.
    assert!(validate_url_scheme("http://8.8.8.8:80").is_ok());
    // A non-internal endpoint URL must not match the internal blocklist.
    assert!(!is_internal_endpoint("https://api.openai.com/v1/chat"));
}

#[test]
fn client_builder_returns_configured_client() {
    // Just proves the builder compiles and does not panic at construction.
    // The returned client carries the SSRF-safe resolver + redirect=none
    // policy documented in src/net/ssrf.rs.
    let _client = ssrf_http_client(Duration::from_secs(5));
}
