//! Phase 64 SEC-01 — webhook outbound SSRF contract.
//!
//! **Note on scope:** this repo ships an INBOUND webhook receiver
//! (`src/gateway/handlers/webhooks.rs`) but does NOT currently ship an
//! outbound webhook delivery client (the original Plan 02 spec assumed one
//! at `src/webhook.rs`, which does not exist — see 64-02-SUMMARY.md
//! "Deviations from Plan" for the full discovery note). Rather than invent
//! a delivery API just to satisfy the plan, we lock the SHARED SSRF-GUARD
//! CONTRACT here so any future webhook outbound path (likely added by
//! Phase 65 OBS plans or a later Phase 64 iteration) has a single
//! canonical gate it MUST go through.
//!
//! What this binary proves:
//!
//! 1. `validate_url_scheme` rejects a loopback-targeting webhook URL with
//!    the typed `SsrfError::InternalBlocked` variant — mapping to HTTP 400
//!    at the gateway layer.
//! 2. `preflight_resolve` rejects DNS names that resolve to private IPs
//!    (loopback used as a deterministic rebind proxy here — every OS
//!    resolver returns 127.0.0.1 for "localhost").
//! 3. `preflight_resolve` passes for a public target so the contract is
//!    observably one-sided: private → reject, public → OK.
//! 4. `ssrf_http_client` is the canonical builder any future webhook
//!    outbound client MUST consume (zero-cost shared resolver).

mod support;

use std::time::Duration;

use opex_core::net::ssrf::{
    is_internal_endpoint, preflight_resolve, ssrf_http_client, validate_url_scheme, SsrfError,
};

#[tokio::test]
async fn webhook_url_pointing_at_loopback_is_rejected() {
    // Classic SSRF: user registers a webhook delivery URL that happens to
    // be the host's own loopback. Must be rejected before any socket opens.
    let res = validate_url_scheme("http://127.0.0.1:9000/hook");
    assert!(
        matches!(res, Err(SsrfError::InternalBlocked(_))),
        "loopback webhook URL must be rejected; got {res:?}"
    );
}

#[tokio::test]
async fn webhook_url_pointing_at_docker_socket_is_rejected() {
    // Docker API on 2375 is in the internal blocklist — catches the
    // common "hit the Docker API via webhook" pivot.
    let res = validate_url_scheme("http://localhost:2375/v1.40/containers/json");
    assert!(
        matches!(res, Err(SsrfError::InternalBlocked(_))),
        "Docker socket webhook URL must be rejected; got {res:?}"
    );
}

#[tokio::test]
async fn webhook_url_pointing_at_rfc1918_is_rejected() {
    let res = validate_url_scheme("http://192.168.1.1:8080/hook");
    assert!(
        matches!(res, Err(SsrfError::InternalBlocked(_))),
        "RFC1918 webhook URL must be rejected; got {res:?}"
    );
}

#[tokio::test]
async fn webhook_url_pointing_at_teredo_range_is_rejected() {
    // Phase 64 CONTEXT.md explicitly added Teredo (2001::/32) to the
    // private-IP set. A webhook URL pointing there must be refused.
    let res = validate_url_scheme("http://[2001::1]/hook");
    assert!(
        matches!(res, Err(SsrfError::InternalBlocked(_))),
        "Teredo webhook URL must be rejected; got {res:?}"
    );
}

#[tokio::test]
async fn webhook_url_dns_rebind_is_rejected() {
    // `localhost` always resolves to 127.0.0.1 on every supported host —
    // this acts as a deterministic rebind proxy without needing to spin up
    // a malicious DNS fixture for the webhook path. The same DnsRebinding
    // fixture in tests/support/dns_fixture.rs is used by
    // integration_ssrf_guard.rs for the end-to-end reqwest client test.
    let res = preflight_resolve("localhost").await;
    assert!(
        matches!(res, Err(SsrfError::PrivateIpResolved)),
        "localhost preflight must fail closed; got {res:?}"
    );
}

#[tokio::test]
async fn webhook_public_target_passes_preflight() {
    // Baseline: a public host (a public literal IP here to avoid flakiness
    // on CI machines without DNS) must not be rejected by the preflight.
    // Regression check: the guard must be one-sided.
    assert!(preflight_resolve("8.8.8.8").await.is_ok());
    assert!(preflight_resolve("1.1.1.1").await.is_ok());
}

#[tokio::test]
async fn webhook_non_http_scheme_is_rejected() {
    // Must not be coerced into file:// or gopher:// via a crafted webhook URL.
    assert!(matches!(
        validate_url_scheme("file:///etc/passwd"),
        Err(SsrfError::BlockedScheme(_))
    ));
    assert!(matches!(
        validate_url_scheme("gopher://evil.example/"),
        Err(SsrfError::BlockedScheme(_))
    ));
}

#[test]
fn webhook_internal_endpoint_predicate_is_consistent() {
    // The same predicate YAML tools use — future webhook outbound code
    // MUST NOT disagree with it, hence the explicit contract test.
    assert!(is_internal_endpoint("http://localhost:9011/describe-url"));
    assert!(is_internal_endpoint("http://searxng:8080/search"));
    assert!(!is_internal_endpoint("https://hooks.slack.com/services/T0/B0/ZZ"));
    assert!(!is_internal_endpoint("https://example.com/webhook"));
}

#[test]
fn webhook_canonical_client_builder_compiles() {
    // Future webhook outbound delivery code is expected to use this
    // builder. The test here locks the ABI: timeout argument, returns a
    // ready-to-use `reqwest::Client` with the SSRF-safe DNS resolver.
    let _client: reqwest::Client = ssrf_http_client(Duration::from_secs(15));
}
