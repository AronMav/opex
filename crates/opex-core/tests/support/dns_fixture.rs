//! DNS rebinding fixture for Phase 64 SEC-01 SSRF tests.
//!
//! Returns `first_ip` on the first `resolve()` call and `second_ip` on every
//! subsequent call. This simulates a malicious authoritative DNS server whose
//! first answer is a public IP (passes a one-shot preflight validation) but
//! whose second answer is a private IP (hits loopback / RFC 1918 when the
//! HTTP client actually connects).
//!
//! Wave 1 Plan 02 (SEC-01) uses this fixture to prove that
//! `SsrfSafeResolver` filters every DNS call — not just the preflight one —
//! closing the TOCTOU gap a cache-once design would leave open.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A `reqwest::dns::Resolve` implementation that flips its answer after the
/// first lookup. Use it as the DNS backend for a test `reqwest::Client` to
/// emulate DNS-rebinding attacks.
pub struct DnsRebindingResolver {
    /// Hostname the resolver is scoped to. For informational/logging use
    /// only — `reqwest` looks up by the `Name` argument passed to `resolve()`.
    pub hostname: String,
    /// IP returned on the very first call (typically a public address).
    pub first_ip: IpAddr,
    /// IP returned on every subsequent call (typically a private/loopback
    /// address intended to be blocked by the production SSRF resolver).
    pub second_ip: IpAddr,
    call_count: AtomicUsize,
}

impl DnsRebindingResolver {
    /// Construct a resolver that returns `first_ip` once, then `second_ip`
    /// forever.
    pub fn new(hostname: &str, first_ip: IpAddr, second_ip: IpAddr) -> Self {
        Self {
            hostname: hostname.to_string(),
            first_ip,
            second_ip,
            call_count: AtomicUsize::new(0),
        }
    }

    /// How many times `resolve()` has been invoked. Handy for asserting that
    /// the client under test did NOT cache the first answer.
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl reqwest::dns::Resolve for DnsRebindingResolver {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let n = self.call_count.fetch_add(1, Ordering::SeqCst);
        let ip = if n == 0 { self.first_ip } else { self.second_ip };
        let addrs: Vec<SocketAddr> = vec![SocketAddr::new(ip, 0)];
        let iter = Box::new(addrs.into_iter()) as reqwest::dns::Addrs;
        Box::pin(async move {
            Ok::<reqwest::dns::Addrs, Box<dyn std::error::Error + Send + Sync>>(iter)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::dns::{Name, Resolve};
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    fn name(hostname: &str) -> Name {
        Name::from_str(hostname).expect("valid DNS name")
    }

    #[tokio::test]
    async fn flips_after_first_lookup() {
        let public = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)); // example.com
        let private = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let r = DnsRebindingResolver::new("evil.example", public, private);

        let mut first = r.resolve(name("evil.example")).await.expect("first resolve ok");
        let first_addr = first.next().expect("first addr").ip();
        assert_eq!(first_addr, public, "first call must return public IP");

        let mut second = r.resolve(name("evil.example")).await.expect("second resolve ok");
        let second_addr = second.next().expect("second addr").ip();
        assert_eq!(second_addr, private, "second call must return private IP");

        let mut third = r.resolve(name("evil.example")).await.expect("third resolve ok");
        let third_addr = third.next().expect("third addr").ip();
        assert_eq!(third_addr, private, "subsequent calls stay on private IP");

        assert_eq!(r.call_count(), 3, "call_count tracks every resolve()");
    }
}
