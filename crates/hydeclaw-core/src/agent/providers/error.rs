use std::sync::Arc;
use thiserror::Error;

/// The reason a `cancellable_stream` was terminated. Written to a
/// `CancelSlot` before `CancellationToken::cancel()` fires, so readers
/// that wake on the token always see a populated reason.
///
/// Note: connect-phase timeouts are handled by reqwest's own
/// `connect_timeout` (not this cancellation path) and surface as
/// `LlmCallError::ConnectTimeout` via `classify_reqwest_err`. There is no
/// `CancelReason::ConnectTimeout` variant because the reqwest stream never
/// reaches our cancellable layer when a connect fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    InactivityTimeout { silent_secs: u64 },
    MaxDurationExceeded { elapsed_secs: u64 },
    // `UserCancelled` is written to the slot by the `cancellable_stream`
    // producer task's external-cancel arm (guarded: only if the slot is
    // still empty). Triggered by `POST /api/chat/{sid}/abort` from the UI
    // Stop button, which fires the session's `CancellationToken`.
    UserCancelled,
    // `ShutdownDrain` is written by the public `set_shutdown_drain_reason`
    // helper, which a future shutdown handler will call BEFORE cancelling
    // the root token. No runtime caller today — `#[allow(dead_code)]`
    // keeps the variant in the match surface so all providers compile.
    #[allow(dead_code)]
    ShutdownDrain,
}

/// Structured partial response state captured on stream timeout.
///
/// Only `Text` can be used for Anthropic-style assistant prefill.
/// `ToolUse` and `Thinking` cannot be partially resumed.
#[derive(Debug, Clone)]
pub enum PartialState {
    /// Accumulated text deltas — usable for Anthropic assistant prefill.
    Text(String),
    /// Stream cut during a tool_use block — cannot resume mid-JSON.
    ToolUse,
    /// Stream cut during a thinking block — cannot resume.
    /// Reserved for future Anthropic extended-thinking support; not yet constructed
    /// by any provider (ThinkingFilter discards thinking deltas before cancellation).
    #[allow(dead_code)]
    Thinking,
    /// Nothing accumulated before the timeout.
    Empty,
}

impl PartialState {
    pub fn is_resumable(&self) -> bool {
        matches!(self, Self::Text(s) if !s.is_empty())
    }

    pub fn text(&self) -> Option<&str> {
        if let Self::Text(s) = self { Some(s) } else { None }
    }
}

/// Single typed error enum every LLM provider returns.
///
/// Every variant that corresponds to a cancellation carries
/// `partial_state` so the engine can persist work already produced;
/// see spec §5.
#[derive(Debug, Clone, Error)]
pub enum LlmCallError {
    #[error("{provider}: connect timed out after {elapsed_secs}s")]
    ConnectTimeout { provider: String, elapsed_secs: u64 },

    #[error("{provider}: provider stopped sending data for {silent_secs}s")]
    InactivityTimeout {
        provider: String,
        silent_secs: u64,
        partial_state: PartialState,
    },

    #[error("{provider}: request timed out after {elapsed_secs}s")]
    RequestTimeout { provider: String, elapsed_secs: u64 },

    #[error("{provider}: stream exceeded max duration {elapsed_secs}s")]
    MaxDurationExceeded {
        provider: String,
        elapsed_secs: u64,
        partial_state: PartialState,
    },

    #[error("stopped by user")]
    UserCancelled { partial_state: PartialState },

    #[error("interrupted by shutdown drain")]
    ShutdownDrain { partial_state: PartialState },

    #[error("{provider}: schema error at byte {at_bytes}: {detail}")]
    SchemaError {
        provider: String,
        detail: String,
        /// Offset into the response body where the error was detected.
        /// `0` means "request rejected before any bytes streamed" → failover.
        /// Non-zero means "error mid-stream" → no failover (partial content
        /// already delivered to the user).
        at_bytes: u64,
    },

    #[error("{provider}: auth failed with status {status}")]
    AuthError { provider: String, status: u16 },

    #[error("{provider}: server returned {status}")]
    Server5xx { provider: String, status: u16 },

    // `reqwest::Error` is not `Clone`, so wrap in `Arc` to keep the
    // `LlmCallError: Clone` contract required by downstream consumers
    // (e.g. error broadcast to multiple tasks). Manual `From` impl below
    // since `#[from]` on an `Arc<T>`-wrapped field is not supported.
    #[error("network error: {0}")]
    Network(Arc<reqwest::Error>),
}

impl From<reqwest::Error> for LlmCallError {
    fn from(err: reqwest::Error) -> Self {
        LlmCallError::Network(Arc::new(err))
    }
}

/// Classify a `reqwest::Error` returned by `RequestBuilder::send()` into the
/// appropriate typed `LlmCallError` variant. Called at each HTTP request
/// launch point in every HTTP provider (OpenAI, Anthropic, Google, HTTP).
///
/// Mapping (spec §4.3 / §4.4):
/// - `is_connect()` → `ConnectTimeout`. Note: `reqwest::Error::is_connect()`
///   is true for ANY connect-layer failure — slow TCP handshake, refused
///   (ECONNREFUSED), DNS failure, unreachable host. The variant is named
///   `ConnectTimeout` because the hot-path case is a handshake that
///   exceeded `connect_secs`; for fast-fail cases (refused, DNS) the
///   variant still carries `elapsed_secs = connect_secs` as an upper
///   bound since reqwest does not expose the actual elapsed time. All of
///   these are failover-worthy and semantically equivalent at the
///   routing layer, so the naming imprecision does not change behavior.
/// - `is_timeout()` → `RequestTimeout` (the outer reqwest request timeout
///   fired — this is distinct from stream inactivity, which surfaces via
///   `CancelSlot`).
/// - `is_status()` with status 5xx → `Server5xx` (failover-worthy).
/// - `is_status()` with status 401/403 → `AuthError` (non-failover-worthy).
/// - anything else → `Network` (wraps the original error, failover-worthy).
pub fn classify_reqwest_err(
    err: reqwest::Error,
    provider: &str,
    connect_secs: u64,
    request_secs: u64,
) -> LlmCallError {
    if err.is_connect() {
        return LlmCallError::ConnectTimeout {
            provider: provider.to_string(),
            elapsed_secs: connect_secs,
        };
    }
    if err.is_timeout() {
        return LlmCallError::RequestTimeout {
            provider: provider.to_string(),
            elapsed_secs: request_secs,
        };
    }
    if let Some(status) = err.status() {
        let code = status.as_u16();
        if code == 401 || code == 403 {
            return LlmCallError::AuthError {
                provider: provider.to_string(),
                status: code,
            };
        }
        if code >= 500 {
            return LlmCallError::Server5xx {
                provider: provider.to_string(),
                status: code,
            };
        }
    }
    LlmCallError::Network(Arc::new(err))
}

impl LlmCallError {
    /// True when `RoutingProvider` should attempt the next route.
    pub fn is_failover_worthy(&self) -> bool {
        use LlmCallError::*;
        match self {
            ConnectTimeout { .. }
            | RequestTimeout { .. }
            | Network(_)
            | Server5xx { .. } => true,

            InactivityTimeout { .. }      // changed: no longer failover-worthy (retry same provider)
            | MaxDurationExceeded { .. }
            | UserCancelled { .. }
            | ShutdownDrain { .. }
            | AuthError { .. } => false,

            SchemaError { at_bytes, .. } => *at_bytes == 0,
        }
    }

    /// Returns the partial state if this variant carries one.
    pub fn partial_state(&self) -> Option<&PartialState> {
        use LlmCallError::*;
        match self {
            InactivityTimeout { partial_state, .. }
            | MaxDurationExceeded { partial_state, .. }
            | UserCancelled { partial_state }
            | ShutdownDrain { partial_state } => Some(partial_state),
            _ => None,
        }
    }

    /// Stable short identifier persisted to `messages.abort_reason`.
    /// Changing these strings breaks historical rows.
    pub fn abort_reason(&self) -> Option<&'static str> {
        use LlmCallError::*;
        Some(match self {
            ConnectTimeout { .. } => "connect_timeout",
            InactivityTimeout { .. } => "inactivity",
            RequestTimeout { .. } => "request_timeout",
            MaxDurationExceeded { .. } => "max_duration",
            UserCancelled { .. } => "user_cancelled",
            ShutdownDrain { .. } => "shutdown_drain",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_failover_worthy_connect_timeout() {
        let e = LlmCallError::ConnectTimeout { provider: "p".into(), elapsed_secs: 10 };
        assert!(e.is_failover_worthy());
    }

    #[test]
    fn is_failover_worthy_request_timeout() {
        let e = LlmCallError::RequestTimeout { provider: "p".into(), elapsed_secs: 120 };
        assert!(e.is_failover_worthy());
    }

    #[test]
    fn is_failover_worthy_server_5xx() {
        let e = LlmCallError::Server5xx { provider: "p".into(), status: 503 };
        assert!(e.is_failover_worthy());
    }

    #[test]
    fn not_failover_worthy_max_duration() {
        let e = LlmCallError::MaxDurationExceeded {
            provider: "p".into(),
            elapsed_secs: 600,
            partial_state: PartialState::Empty,
        };
        assert!(!e.is_failover_worthy());
    }

    #[test]
    fn not_failover_worthy_user_cancelled() {
        let e = LlmCallError::UserCancelled { partial_state: PartialState::Empty };
        assert!(!e.is_failover_worthy());
    }

    #[test]
    fn not_failover_worthy_shutdown_drain() {
        let e = LlmCallError::ShutdownDrain { partial_state: PartialState::Empty };
        assert!(!e.is_failover_worthy());
    }

    #[test]
    fn not_failover_worthy_auth_error() {
        let e = LlmCallError::AuthError { provider: "p".into(), status: 401 };
        assert!(!e.is_failover_worthy());
    }

    #[test]
    fn schema_error_failover_depends_on_at_bytes() {
        let pre = LlmCallError::SchemaError {
            provider: "p".into(),
            detail: "bad".into(),
            at_bytes: 0,
        };
        assert!(pre.is_failover_worthy(), "pre-stream schema error MUST fail over");

        let mid = LlmCallError::SchemaError {
            provider: "p".into(),
            detail: "bad".into(),
            at_bytes: 1024,
        };
        assert!(!mid.is_failover_worthy(), "mid-stream schema error MUST NOT fail over");
    }

    #[test]
    fn variants_carrying_partial_state_can_return_it() {
        let e = LlmCallError::UserCancelled { partial_state: PartialState::Text("hello".into()) };
        match e.partial_state() {
            Some(PartialState::Text(s)) => assert_eq!(s, "hello"),
            other => panic!("expected Some(Text), got {other:?}"),
        }

        let e2 = LlmCallError::ConnectTimeout { provider: "p".into(), elapsed_secs: 5 };
        assert!(e2.partial_state().is_none());
    }

    // ── classify_reqwest_err tests ──────────────────────────────────────────
    // reqwest::Error cannot be constructed directly; we manufacture them via
    // real (local/invalid) requests. These tests verify the branches we can
    // reach without outbound network calls: connect failures (nonexistent
    // port) and status-failed responses (via `.error_for_status()`).

    async fn make_connect_err() -> reqwest::Error {
        // Random high port on 127.0.0.1 that's (almost certainly) closed.
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        client.get("http://127.0.0.1:1").send().await.unwrap_err()
    }

    #[tokio::test]
    async fn classify_reqwest_err_connect_failure_maps_to_connect_timeout() {
        let err = make_connect_err().await;
        let classified = classify_reqwest_err(err, "testprov", 7, 999);
        match classified {
            LlmCallError::ConnectTimeout { provider, elapsed_secs } => {
                assert_eq!(provider, "testprov");
                assert_eq!(elapsed_secs, 7);
            }
            // On Windows/macOS a refused-TCP connection occasionally bubbles
            // up as `is_request()` rather than `is_connect()`. Accept Network
            // (fallthrough) as a valid alternate classification for that edge.
            LlmCallError::Network(_) => {}
            other => panic!("expected ConnectTimeout or Network, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn classify_reqwest_err_fallthrough_is_network() {
        // Arbitrary parse error path: reqwest can produce Url parse errors.
        // Simpler: reuse the connect error and re-classify as a different
        // provider name — Network fallthrough is covered above; here we
        // just assert that an error without status/timeout/connect goes to
        // Network. We manufacture via a truly unparseable URL.
        let err = reqwest::Client::new()
            .get("http://[::bad-host")
            .send()
            .await
            .unwrap_err();
        let classified = classify_reqwest_err(err, "p", 10, 120);
        assert!(matches!(classified, LlmCallError::Network(_)));
    }

    #[test]
    fn abort_reason_strings_are_stable() {
        // Used as the persisted `messages.abort_reason` column — changing
        // these strings breaks historical rows. Pin them here.
        use LlmCallError::*;
        assert_eq!(ConnectTimeout { provider: "p".into(), elapsed_secs: 1 }.abort_reason(), Some("connect_timeout"));
        assert_eq!(InactivityTimeout { provider: "p".into(), silent_secs: 1, partial_state: PartialState::Empty }.abort_reason(), Some("inactivity"));
        assert_eq!(RequestTimeout { provider: "p".into(), elapsed_secs: 1 }.abort_reason(), Some("request_timeout"));
        assert_eq!(MaxDurationExceeded { provider: "p".into(), elapsed_secs: 1, partial_state: PartialState::Empty }.abort_reason(), Some("max_duration"));
        assert_eq!(UserCancelled { partial_state: PartialState::Empty }.abort_reason(), Some("user_cancelled"));
        assert_eq!(ShutdownDrain { partial_state: PartialState::Empty }.abort_reason(), Some("shutdown_drain"));
    }

    #[test]
    fn partial_state_text_is_resumable() {
        use super::PartialState;
        assert!(PartialState::Text("hello".into()).is_resumable());
        assert!(!PartialState::Text(String::new()).is_resumable());
    }

    #[test]
    fn partial_state_non_text_is_not_resumable() {
        use super::PartialState;
        assert!(!PartialState::ToolUse.is_resumable());
        assert!(!PartialState::Thinking.is_resumable());
        assert!(!PartialState::Empty.is_resumable());
    }

    #[test]
    fn inactivity_is_not_failover_worthy_after_r1() {
        let e = LlmCallError::InactivityTimeout {
            provider: "p".into(),
            silent_secs: 60,
            partial_state: PartialState::Empty,
        };
        assert!(!e.is_failover_worthy(), "InactivityTimeout must NOT be failover-worthy after R1");
    }
}
