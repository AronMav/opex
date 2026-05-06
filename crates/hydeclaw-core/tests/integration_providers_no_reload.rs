//! T5 — verify the toolgate `/reload` push path is fully gone.
//!
//! The full S5 spec calls for a wiremock-based test that spins up Core,
//! POSTs to provider/secret endpoints, and asserts no outbound `/reload`
//! traffic. Doing that here would require a mocked Core router fixture
//! that does not currently exist (every existing integration test either
//! drives PG via testcontainers or stubs out subsystems individually —
//! none drive the Axum router end-to-end against a wiremock toolgate).
//!
//! Building that fixture is out of scope for T5 (it would change the
//! shape of the test harness, not just T5's surface area). Instead, this
//! file pins the contract via source-level invariants: Core source must
//! not contain the legacy reload symbol or URL path. If anyone re-adds
//! either, this test fails — same end goal as the wiremock test, with
//! zero infrastructure cost.
//!
//! Toolgate-side coverage of the new pull-on-call semantics lives in
//! `toolgate/tests/test_registry.py::test_aget_active_calls_core_api_each_time`
//! and friends.

const PROVIDERS_RS: &str = include_str!("../src/gateway/handlers/providers.rs");
const SECRETS_RS: &str = include_str!("../src/gateway/handlers/secrets.rs");

#[test]
fn no_notify_toolgate_reload_function_or_call_in_providers() {
    assert!(
        !PROVIDERS_RS.contains("notify_toolgate_reload"),
        "notify_toolgate_reload must be removed from providers.rs (T5 — pull-on-call)"
    );
}

#[test]
fn no_notify_toolgate_reload_call_in_secrets_set() {
    assert!(
        !SECRETS_RS.contains("notify_toolgate_reload"),
        "notify_toolgate_reload must be removed from secrets.rs (T5 — pull-on-call)"
    );
}

#[test]
fn no_outbound_reload_path_in_providers() {
    // Catches accidental re-introduction of `/reload` POST URLs anywhere
    // in providers.rs (the old code formatted `{url}/reload`).
    assert!(
        !PROVIDERS_RS.contains("/reload"),
        "providers.rs must not POST to toolgate /reload (T5 — endpoint deleted)"
    );
}

#[test]
fn no_outbound_reload_path_in_secrets() {
    assert!(
        !SECRETS_RS.contains("/reload"),
        "secrets.rs must not POST to toolgate /reload (T5 — endpoint deleted)"
    );
}
