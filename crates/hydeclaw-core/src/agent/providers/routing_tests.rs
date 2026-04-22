//! Tests for `RoutingProvider` failover behavior driven by
//! `LlmCallError::is_failover_worthy`. See Tasks 17/18 of the LLM-timeout
//! refactor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use hydeclaw_types::{LlmResponse, Message, ToolDefinition};
use tokio::sync::mpsc;

use super::{LlmCallError, LlmProvider, RoutingProvider};

// ── Mock providers ───────────────────────────────────────────────────────────

/// Always returns `Server5xx` (failover-worthy). Used to test failover paths
/// that previously used InactivityTimeout — after R1, InactivityTimeout is
/// no longer failover-worthy (it retries the same provider instead).
struct MockFailoverProvider;

#[async_trait]
impl LlmProvider for MockFailoverProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        Err(anyhow::Error::new(LlmCallError::Server5xx {
            provider: "mock-failover".into(),
            status: 503,
        }))
    }

    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _chunk_tx: mpsc::UnboundedSender<String>,
    ) -> anyhow::Result<LlmResponse> {
        Err(anyhow::Error::new(LlmCallError::Server5xx {
            provider: "mock-failover".into(),
            status: 503,
        }))
    }

    fn name(&self) -> &str {
        "mock-failover"
    }
}

/// Always returns `UserCancelled` (NOT failover-worthy).
struct MockUserCancelProvider;

#[async_trait]
impl LlmProvider for MockUserCancelProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        Err(anyhow::Error::new(LlmCallError::UserCancelled {
            partial_state: crate::agent::providers::error::PartialState::Text("partial-before-cancel".into()),
        }))
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _chunk_tx: mpsc::UnboundedSender<String>,
    ) -> anyhow::Result<LlmResponse> {
        self.chat(messages, tools).await
    }

    fn name(&self) -> &str {
        "mock-user-cancel"
    }
}

/// Always returns `AuthError` (NOT failover-worthy — typed path).
struct MockAuthErrorProvider;

#[async_trait]
impl LlmProvider for MockAuthErrorProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        Err(anyhow::Error::new(LlmCallError::AuthError {
            provider: "mock-auth".into(),
            status: 401,
        }))
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _chunk_tx: mpsc::UnboundedSender<String>,
    ) -> anyhow::Result<LlmResponse> {
        self.chat(messages, tools).await
    }

    fn name(&self) -> &str {
        "mock-auth"
    }
}

/// Records whether it was called and returns success with a distinctive content.
struct MockSuccessProvider {
    called: Arc<AtomicBool>,
    marker: &'static str,
}

#[async_trait]
impl LlmProvider for MockSuccessProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        self.called.store(true, Ordering::SeqCst);
        Ok(LlmResponse {
            content: self.marker.to_string(),
            tool_calls: vec![],
            usage: None,
            finish_reason: Some("stop".to_string()),
            model: None,
            provider: Some("mock-success".to_string()),
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        })
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _chunk_tx: mpsc::UnboundedSender<String>,
    ) -> anyhow::Result<LlmResponse> {
        self.chat(messages, tools).await
    }

    fn name(&self) -> &str {
        "mock-success"
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// After R1, `Server5xx` is failover-worthy and triggers failover to the next route.
#[tokio::test]
async fn routing_fails_over_on_server_error() {
    let called = Arc::new(AtomicBool::new(false));
    let primary: Arc<dyn LlmProvider> = Arc::new(MockFailoverProvider);
    let fallback: Arc<dyn LlmProvider> = Arc::new(MockSuccessProvider {
        called: called.clone(),
        marker: "from-fallback",
    });

    let routing = RoutingProvider::new_for_test(vec![
        ("primary:mock-failover".into(), primary, 60),
        ("fallback:mock-success".into(), fallback, 60),
    ]);

    let resp = routing.chat(&[], &[]).await.expect("failover should succeed");
    assert_eq!(resp.content, "from-fallback");
    assert!(called.load(Ordering::SeqCst), "fallback must have been called");
}

/// After R1, `InactivityTimeout` is NOT failover-worthy — it bubbles up for retry.
#[tokio::test]
async fn routing_does_not_fail_over_on_inactivity_timeout() {
    use crate::agent::providers::error::PartialState;

    let called = Arc::new(AtomicBool::new(false));

    struct MockInactivityProvider;
    #[async_trait]
    impl LlmProvider for MockInactivityProvider {
        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<LlmResponse> {
            Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                provider: "mock-inactivity".into(),
                silent_secs: 60,
                partial_state: PartialState::Text("partial".into()),
            }))
        }
        async fn chat_stream(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
            _chunk_tx: mpsc::UnboundedSender<String>,
        ) -> anyhow::Result<LlmResponse> {
            self.chat(messages, tools).await
        }
        fn name(&self) -> &str { "mock-inactivity" }
    }

    let primary: Arc<dyn LlmProvider> = Arc::new(MockInactivityProvider);
    let fallback: Arc<dyn LlmProvider> = Arc::new(MockSuccessProvider {
        called: called.clone(),
        marker: "from-fallback",
    });

    let routing = RoutingProvider::new_for_test(vec![
        ("primary:mock-inactivity".into(), primary, 60),
        ("fallback:mock-success".into(), fallback, 60),
    ]);

    let err = routing
        .chat(&[], &[])
        .await
        .expect_err("InactivityTimeout must bubble up after R1, not fail over");
    let typed = err.downcast_ref::<LlmCallError>().expect("error must be LlmCallError");
    assert!(
        matches!(typed, LlmCallError::InactivityTimeout { .. }),
        "expected InactivityTimeout, got {typed:?}"
    );
    assert!(
        !called.load(Ordering::SeqCst),
        "fallback MUST NOT be called for non-failover-worthy InactivityTimeout"
    );
}

#[tokio::test]
async fn routing_does_not_fail_over_on_user_cancel() {
    let called = Arc::new(AtomicBool::new(false));
    let primary: Arc<dyn LlmProvider> = Arc::new(MockUserCancelProvider);
    let fallback: Arc<dyn LlmProvider> = Arc::new(MockSuccessProvider {
        called: called.clone(),
        marker: "from-fallback",
    });

    let routing = RoutingProvider::new_for_test(vec![
        ("primary:mock-user-cancel".into(), primary, 60),
        ("fallback:mock-success".into(), fallback, 60),
    ]);

    let err = routing
        .chat(&[], &[])
        .await
        .expect_err("user-cancelled must bubble up, not fail over");
    let typed = err
        .downcast_ref::<LlmCallError>()
        .expect("error must be an LlmCallError");
    assert!(
        matches!(typed, LlmCallError::UserCancelled { .. }),
        "expected UserCancelled, got {typed:?}"
    );
    // Partial state preserved.
    match typed.partial_state() {
        Some(crate::agent::providers::error::PartialState::Text(s)) => assert_eq!(s, "partial-before-cancel"),
        other => panic!("expected Some(Text(\"partial-before-cancel\")), got {other:?}"),
    }
    assert!(
        !called.load(Ordering::SeqCst),
        "fallback MUST NOT have been called for non-failover-worthy error"
    );
}

#[tokio::test]
async fn routing_does_not_fail_over_on_auth_error() {
    let called = Arc::new(AtomicBool::new(false));
    let primary: Arc<dyn LlmProvider> = Arc::new(MockAuthErrorProvider);
    let fallback: Arc<dyn LlmProvider> = Arc::new(MockSuccessProvider {
        called: called.clone(),
        marker: "from-fallback",
    });

    let routing = RoutingProvider::new_for_test(vec![
        ("primary:mock-auth".into(), primary, 60),
        ("fallback:mock-success".into(), fallback, 60),
    ]);

    let err = routing
        .chat(&[], &[])
        .await
        .expect_err("auth error must bubble up, not fail over");
    let typed = err
        .downcast_ref::<LlmCallError>()
        .expect("error must be an LlmCallError");
    assert!(
        matches!(typed, LlmCallError::AuthError { .. }),
        "expected AuthError, got {typed:?}"
    );
    assert!(
        !called.load(Ordering::SeqCst),
        "fallback MUST NOT have been called for auth error"
    );
}

/// LLM-timeout refactor: `InactivityTimeout` is NOT failover-worthy after R1,
/// but the timeout counter IS still bumped on `handle_provider_error`.
///
/// Test isolation note: the `global()` OnceLock is process-wide and tests
/// run in parallel, so we can't read absolute values. Instead we use
/// unique provider names so this test's counter labels are isolated.
#[tokio::test]
async fn routing_bumps_timeout_counter_on_inactivity_but_does_not_fail_over() {
    use std::sync::Arc;
    use crate::agent::providers::error::PartialState;

    // Provider that returns an InactivityTimeout with a unique provider name.
    struct UniqueInactivityProvider;
    #[async_trait]
    impl LlmProvider for UniqueInactivityProvider {
        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<LlmResponse> {
            Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                provider: "mock-inactivity-unique-t22".into(),
                silent_secs: 60,
                partial_state: PartialState::Empty,
            }))
        }
        async fn chat_stream(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
            _chunk_tx: mpsc::UnboundedSender<String>,
        ) -> anyhow::Result<LlmResponse> {
            self.chat(messages, tools).await
        }
        fn name(&self) -> &str {
            "mock-inactivity-unique-t22"
        }
    }

    // Ensure a registry is installed (first-writer-wins; if another test
    // installed one already, we use that — same Arc<MetricsRegistry>).
    let registry = Arc::new(crate::metrics::MetricsRegistry::new());
    crate::metrics::install_global(registry);
    let metrics = crate::metrics::global()
        .expect("global metrics installed")
        .clone();

    let called = Arc::new(AtomicBool::new(false));
    let primary: Arc<dyn LlmProvider> = Arc::new(UniqueInactivityProvider);
    let fallback: Arc<dyn LlmProvider> = Arc::new(MockSuccessProvider {
        called: called.clone(),
        marker: "from-fallback-t22",
    });

    let routing = RoutingProvider::new_for_test(vec![
        ("primary:unique-t22".into(), primary, 60),
        ("fallback:unique-t22".into(), fallback, 60),
    ]);

    // After R1: InactivityTimeout bubbles up, fallback is NOT called.
    let err = routing
        .chat(&[], &[])
        .await
        .expect_err("InactivityTimeout must bubble up after R1");
    let typed = err.downcast_ref::<LlmCallError>().expect("must be LlmCallError");
    assert!(matches!(typed, LlmCallError::InactivityTimeout { .. }));
    assert!(
        !called.load(Ordering::SeqCst),
        "fallback MUST NOT be called for non-failover-worthy InactivityTimeout"
    );

    // Unique labels → exact-equality assertion is safe even under parallel
    // test execution. Timeout counter IS bumped even for non-failover errors.
    let timeout_snap = metrics.snapshot_llm_timeout_total();
    assert_eq!(
        timeout_snap.get(&(
            "mock-inactivity-unique-t22".to_string(),
            "inactivity".to_string()
        )),
        Some(&1),
        "llm_timeout_total{{provider=mock-inactivity-unique-t22,kind=inactivity}} must be 1"
    );
    // Failover counter is NOT bumped — inactivity is no longer failover-worthy.
    let failover_snap = metrics.snapshot_llm_failover_total();
    assert_eq!(
        failover_snap.get(&(
            "primary:unique-t22".to_string(),
            "fallback:unique-t22".to_string(),
            "inactivity".to_string()
        )),
        None,
        "llm_failover_total must NOT be bumped for non-failover-worthy InactivityTimeout"
    );
}

#[tokio::test]
async fn routing_fails_over_on_streaming_server_error() {
    let called = Arc::new(AtomicBool::new(false));
    let primary: Arc<dyn LlmProvider> = Arc::new(MockFailoverProvider);
    let fallback: Arc<dyn LlmProvider> = Arc::new(MockSuccessProvider {
        called: called.clone(),
        marker: "streamed-fallback",
    });

    let routing = RoutingProvider::new_for_test(vec![
        ("primary:mock-failover".into(), primary, 60),
        ("fallback:mock-success".into(), fallback, 60),
    ]);

    let (tx, _rx) = mpsc::unbounded_channel::<String>();
    let resp = routing
        .chat_stream(&[], &[], tx)
        .await
        .expect("streaming failover should succeed");
    assert_eq!(resp.content, "streamed-fallback");
    assert!(called.load(Ordering::SeqCst));
}

// ── Issue #9: max_failover_attempts cap ──────────────────────────────────────

/// Counts how many times it's invoked and always returns a failover-worthy error.
/// Uses `Server5xx` (failover-worthy) — NOT InactivityTimeout, which is no
/// longer failover-worthy after R1.
struct CountingFailoverProvider {
    calls: Arc<std::sync::atomic::AtomicU32>,
    tag: &'static str,
}

#[async_trait]
impl LlmProvider for CountingFailoverProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::Error::new(LlmCallError::Server5xx {
            provider: self.tag.to_string(),
            status: 503,
        }))
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _chunk_tx: mpsc::UnboundedSender<String>,
    ) -> anyhow::Result<LlmResponse> {
        self.chat(messages, tools).await
    }

    fn name(&self) -> &str {
        self.tag
    }
}

#[tokio::test]
async fn routing_respects_max_failover_attempts_cap() {
    // 5 routes, all failing; cap = 2 failovers means: primary + 2 fallbacks = 3 total calls.
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));

    let make = |tag: &'static str| -> Arc<dyn LlmProvider> {
        Arc::new(CountingFailoverProvider {
            calls: calls.clone(),
            tag,
        })
    };

    let routing = RoutingProvider::new_for_test_with_cap(
        vec![
            ("r0:cap-test".into(), make("cap-test-0"), 60),
            ("r1:cap-test".into(), make("cap-test-1"), 60),
            ("r2:cap-test".into(), make("cap-test-2"), 60),
            ("r3:cap-test".into(), make("cap-test-3"), 60),
            ("r4:cap-test".into(), make("cap-test-4"), 60),
        ],
        2, // cap: 2 failover attempts after primary
    );

    let err = routing
        .chat(&[], &[])
        .await
        .expect_err("all routes failing → error");
    assert!(
        err.to_string().contains("all providers failed") || err.downcast_ref::<LlmCallError>().is_some(),
        "expected bail or last typed error, got {err}"
    );

    // primary (1) + 2 fallbacks = 3 total calls. The cap stops us before r3/r4.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "max_failover_attempts=2 → primary + 2 fallbacks = 3 calls, no more"
    );
}

#[tokio::test]
async fn routing_default_cap_allows_full_chain_when_large() {
    // With u32::MAX cap (default of new_for_test), all 4 routes are tried.
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let make = |tag: &'static str| -> Arc<dyn LlmProvider> {
        Arc::new(CountingFailoverProvider {
            calls: calls.clone(),
            tag,
        })
    };
    let routing = RoutingProvider::new_for_test(vec![
        ("rA:no-cap".into(), make("no-cap-0"), 60),
        ("rB:no-cap".into(), make("no-cap-1"), 60),
        ("rC:no-cap".into(), make("no-cap-2"), 60),
        ("rD:no-cap".into(), make("no-cap-3"), 60),
    ]);
    let _ = routing.chat(&[], &[]).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "no cap → all routes exhausted"
    );
}

// ── Issue #2: zero-route RoutingProvider must not panic ──────────────────────

/// Constructing a `RoutingProvider` with an empty route list and calling
/// `chat()` MUST return an error, not panic via `.expect(...)`. This is the
/// belt-and-suspenders test for `select_route` — production code also
/// guarantees the list is non-empty by installing an `UnconfiguredProvider`
/// sentinel in `create_routing_provider`, but `select_route` itself must
/// fail gracefully.
#[tokio::test]
async fn routing_with_zero_routes_returns_error_not_panic() {
    let routing = RoutingProvider::new_for_test(vec![]);

    let err = routing
        .chat(&[], &[])
        .await
        .expect_err("zero-route chat must return an error, not panic");
    let msg = err.to_string();
    assert!(
        msg.contains("no routes") || msg.contains("RoutingProvider"),
        "error must mention the missing-routes condition: {msg}"
    );
}

/// Same guarantee for the streaming path.
#[tokio::test]
async fn routing_with_zero_routes_streaming_returns_error_not_panic() {
    let routing = RoutingProvider::new_for_test(vec![]);

    let (tx, _rx) = mpsc::unbounded_channel::<String>();
    let err = routing
        .chat_stream(&[], &[], tx)
        .await
        .expect_err("zero-route chat_stream must return an error, not panic");
    let msg = err.to_string();
    assert!(
        msg.contains("no routes") || msg.contains("RoutingProvider"),
        "error must mention the missing-routes condition: {msg}"
    );
}

/// `UnconfiguredProvider` sentinel returns a classified `AuthError` on both
/// `chat()` and `chat_stream()` so callers see a uniform failure shape.
#[tokio::test]
async fn unconfigured_provider_returns_classified_auth_error() {
    use super::UnconfiguredProvider;
    let p = UnconfiguredProvider::new("no usable routes");

    let err = p.chat(&[], &[]).await.expect_err("unconfigured must error");
    let typed = err
        .downcast_ref::<LlmCallError>()
        .expect("must be LlmCallError");
    assert!(
        matches!(typed, LlmCallError::AuthError { .. }),
        "unconfigured sentinel must return AuthError, got {typed:?}"
    );
    // AuthError is not failover-worthy — callers should bubble up, not
    // retry against another (non-existent) route.
    assert!(!typed.is_failover_worthy());

    let (tx, _rx) = mpsc::unbounded_channel::<String>();
    let err_stream = p
        .chat_stream(&[], &[], tx)
        .await
        .expect_err("unconfigured streaming must error");
    let typed_stream = err_stream
        .downcast_ref::<LlmCallError>()
        .expect("stream must be LlmCallError");
    assert!(
        matches!(typed_stream, LlmCallError::AuthError { .. }),
        "unconfigured streaming sentinel must return AuthError, got {typed_stream:?}"
    );
}
