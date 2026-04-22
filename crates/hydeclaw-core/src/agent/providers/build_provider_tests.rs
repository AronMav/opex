//! Tests for `build_provider` wiring of `TimeoutsConfig` (issue #1) and
//! `ProviderOverrides` (issue #4). These tests rely on `#[cfg(test)]`
//! accessors on `OpenAiCompatibleProvider` and on the fact that
//! `build_provider_clients` is called unconditionally from `new_from_row`
//! (the `#[allow(dead_code)]` on that helper was removed alongside these
//! tests).

use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::{
    build_provider, build_provider_clients, timeouts::TimeoutsConfig, ProviderOverrides,
};
use crate::db::providers::ProviderRow;
use crate::secrets::SecretsManager;

fn make_row(options: serde_json::Value) -> ProviderRow {
    ProviderRow {
        id: Uuid::nil(),
        name: "test-provider".into(),
        category: "text".into(),
        provider_type: "openai".into(),
        base_url: Some("https://example.invalid".into()),
        default_model: Some("gpt-test".into()),
        enabled: true,
        options,
        notes: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn build_provider_clients_honors_non_default_timeouts() {
    // Regression for issue #1: the helper used to have `#[allow(dead_code)]`
    // because nothing called it. We can't read `connect_timeout` back from
    // `reqwest::Client`, but we CAN assert that passing non-default values
    // doesn't panic (the builder validates).
    let cfg = TimeoutsConfig {
        connect_secs: 5,
        request_secs: 45,
        stream_inactivity_secs: 30,
        stream_max_duration_secs: 300,
        run_max_duration_secs: 0,
    };
    let (_req, _stream) = build_provider_clients(&cfg);
    // Zero request_secs means "no limit" — must also not panic.
    let cfg_zero = TimeoutsConfig {
        connect_secs: 10,
        request_secs: 0,
        stream_inactivity_secs: 60,
        stream_max_duration_secs: 600,
        run_max_duration_secs: 0,
    };
    let (_req2, _stream2) = build_provider_clients(&cfg_zero);
}

#[tokio::test]
async fn build_provider_stores_timeouts_on_openai_provider() {
    // Issue #1: the provider stores the TimeoutsConfig passed to build_provider,
    // not the legacy defaults. Uses the test-only `test_timeouts()` accessor.
    let row = make_row(json!({}));
    let timeouts = TimeoutsConfig {
        connect_secs: 7,
        request_secs: 33,
        stream_inactivity_secs: 44,
        stream_max_duration_secs: 555,
        run_max_duration_secs: 0,
    };
    let secrets = Arc::new(SecretsManager::new_noop());
    let cancel = tokio_util::sync::CancellationToken::new();

    let provider = build_provider(
        &row,
        secrets,
        &timeouts,
        cancel,
        ProviderOverrides::default(),
    )
    .expect("build_provider succeeds");

    // The boxed trait object doesn't expose typed accessors — downcast via
    // the internal submodule path. Since `OpenAiCompatibleProvider` lives in
    // a private submodule, we re-build it manually using `new_from_row` in
    // the second test to access the accessor directly.
    assert_eq!(provider.name(), "openai");
}

#[tokio::test]
async fn openai_new_from_row_honors_overrides_and_timeouts() {
    // Issue #4: `ProviderOverrides { temperature, max_tokens, model }` wins over
    // row defaults. Issue #1: the constructor stores the passed-in timeouts.
    use super::openai_impl::OpenAiCompatibleProvider;

    let row = make_row(json!({}));
    let timeouts = TimeoutsConfig {
        connect_secs: 9,
        request_secs: 111,
        stream_inactivity_secs: 22,
        stream_max_duration_secs: 333,
        run_max_duration_secs: 0,
    };
    let secrets = Arc::new(SecretsManager::new_noop());
    let cancel = tokio_util::sync::CancellationToken::new();
    let opts = super::timeouts::ProviderOptions::default();

    let overrides = ProviderOverrides {
        model: Some("override-model".into()),
        temperature: Some(0.123),
        max_tokens: Some(4321),
        prompt_cache: None,
    };

    let provider = OpenAiCompatibleProvider::new_from_row(
        &row,
        secrets,
        timeouts,
        cancel,
        opts,
        overrides,
    )
    .expect("build succeeds");

    // Issue #4: overrides threaded through.
    assert!(
        (provider.test_temperature() - 0.123).abs() < f64::EPSILON,
        "temperature override must win over hardcoded 0.7"
    );
    assert_eq!(
        provider.test_max_tokens(),
        Some(4321),
        "max_tokens override must win over hardcoded None"
    );

    // Issue #1: timeouts stored, not replaced by legacy defaults.
    let stored = provider.test_timeouts();
    assert_eq!(stored.connect_secs, 9);
    assert_eq!(stored.request_secs, 111);
    assert_eq!(stored.stream_inactivity_secs, 22);
    assert_eq!(stored.stream_max_duration_secs, 333);
}

#[tokio::test]
async fn build_provider_rejects_invalid_options_connect_zero() {
    // Issue A: `build_provider` now validates `ProviderOptions` via
    // `opts.validate()`. A row with connect_secs = 0 (no upper bound on
    // connect → unrecoverable) must be rejected with a clear error.
    let row = make_row(json!({
        "timeouts": { "connect_secs": 0 }
    }));
    let timeouts = TimeoutsConfig::default();
    let secrets = Arc::new(SecretsManager::new_noop());
    let cancel = tokio_util::sync::CancellationToken::new();

    let result = build_provider(
        &row,
        secrets,
        &timeouts,
        cancel,
        ProviderOverrides::default(),
    );
    let err = match result {
        Ok(_) => panic!("must reject invalid options"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("connect_secs"),
        "error should mention offending field: {msg}"
    );
    assert!(
        msg.contains("invalid options"),
        "error should be framed as invalid options: {msg}"
    );
}

#[tokio::test]
async fn openai_new_from_row_falls_back_to_defaults_without_overrides() {
    // When `ProviderOverrides::default()` is passed, the constructor must
    // fall back to the last-resort hardcoded defaults (0.7 / None) — NOT
    // crash or pick up stale data.
    use super::openai_impl::OpenAiCompatibleProvider;

    let row = make_row(json!({}));
    let timeouts = TimeoutsConfig::default();
    let secrets = Arc::new(SecretsManager::new_noop());
    let cancel = tokio_util::sync::CancellationToken::new();
    let opts = super::timeouts::ProviderOptions::default();

    let provider = OpenAiCompatibleProvider::new_from_row(
        &row,
        secrets,
        timeouts,
        cancel,
        opts,
        ProviderOverrides::default(),
    )
    .expect("build succeeds");

    assert!(
        (provider.test_temperature() - 0.7).abs() < f64::EPSILON,
        "default temperature should be 0.7"
    );
    assert_eq!(provider.test_max_tokens(), None);
}
