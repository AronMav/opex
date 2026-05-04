//! LLM error classification for context-specific recovery.
//!
//! Classifies anyhow errors from LLM providers into actionable categories,
//! enabling different recovery strategies (retry, compact, user message, etc.).

use regex::Regex;
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmErrorClass {
    /// Context/prompt too large for the model.
    ContextOverflow,
    /// Orphan tool messages, invalid role ordering — session state is broken.
    SessionCorruption,
    /// Transient server errors (500, 502, 503, 504, 521-524, 529).
    TransientHttp,
    /// Rate limited (429, TPM/RPM exceeded).
    RateLimit,
    /// Authentication permanently failed (invalid/revoked API key).
    AuthPermanent,
    /// Billing/quota issue (402, insufficient credits).
    Billing,
    /// Provider overloaded (capacity, high demand).
    Overloaded,
    /// Stream inactivity or max-duration timeout — handled by the outer deadline retry loop.
    /// NOT retryable by the inner transient retry loop.
    CallTimeout,
    /// Unrecognized error.
    Unknown,
}

// ── Regex patterns (compiled once) ──────────────────────────────────────────

static RE_CONTEXT_OVERFLOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)context.?length|token.?limit|too.many.token|input.too.long|prompt.is.too.long|maximum.context|exceeds.the.model|request_too_large|context.overflow|上下文").unwrap()
});

static RE_SESSION_CORRUPTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)tool_use_block|tool_result.*not.*found|role.*ordering|roles.must.alternate|orphan.*tool|function.call.turn.comes.immediately|incorrect.role.information").unwrap()
});

static RE_TRANSIENT_HTTP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(500|502|503|504|521|522|523|524|529)\b|bad.gateway|gateway.timeout|without.sending.*(chunks?|response)").unwrap()
});

static RE_RATE_LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b429\b|rate.?limit|too.many.requests|tokens.per.minute|\btpm\b|resource.?exhausted|usage.?limit").unwrap()
});

static RE_AUTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\b(401|403)\b.*(api.?key|unauthorized|authentication|forbidden))|(api.?key.*(invalid|revoked|expired|deactivated))|(unauthorized|authentication.*(failed|error))|PERMISSION_DENIED").unwrap()
});

static RE_BILLING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b402\b|payment.required|insufficient.credit|quota.exceeded|insufficient.balance|insufficient.quota|billing").unwrap()
});

static RE_OVERLOADED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)overloaded|service.unavailable.*capacity|high.demand|overloaded_error").unwrap()
});

/// Classify an LLM error into an actionable category.
pub fn classify(error: &anyhow::Error) -> LlmErrorClass {
    // Fast path: typed dispatch for LlmCallError variants.
    if let Some(llm_err) = error.downcast_ref::<crate::agent::providers::error::LlmCallError>() {
        use crate::agent::providers::error::LlmCallError::*;
        match llm_err {
            InactivityTimeout { .. } | MaxDurationExceeded { .. } => return LlmErrorClass::CallTimeout,
            _ => {}
        }
    }
    let msg = error.to_string();
    classify_str(&msg)
}

/// Classify from a raw error string (useful for testing and provider errors).
pub fn classify_str(msg: &str) -> LlmErrorClass {
    // Order matters: more specific patterns first.
    // Billing before rate limit (402 vs 429).
    // Auth before transient (401/403 vs 502).

    if RE_BILLING.is_match(msg) {
        return LlmErrorClass::Billing;
    }
    if RE_AUTH.is_match(msg) {
        return LlmErrorClass::AuthPermanent;
    }
    if RE_CONTEXT_OVERFLOW.is_match(msg) {
        return LlmErrorClass::ContextOverflow;
    }
    if RE_SESSION_CORRUPTION.is_match(msg) {
        return LlmErrorClass::SessionCorruption;
    }
    if RE_RATE_LIMIT.is_match(msg) {
        return LlmErrorClass::RateLimit;
    }
    if RE_OVERLOADED.is_match(msg) {
        return LlmErrorClass::Overloaded;
    }
    if RE_TRANSIENT_HTTP.is_match(msg) {
        return LlmErrorClass::TransientHttp;
    }
    LlmErrorClass::Unknown
}

/// Recommended cooldown duration based on error class.
pub fn cooldown_duration(class: &LlmErrorClass) -> std::time::Duration {
    use std::time::Duration;
    match class {
        LlmErrorClass::AuthPermanent | LlmErrorClass::Billing => Duration::from_secs(3600),
        LlmErrorClass::RateLimit => Duration::from_secs(60),
        LlmErrorClass::Overloaded => Duration::from_secs(30),
        LlmErrorClass::TransientHttp | LlmErrorClass::Unknown => Duration::from_secs(15),
        LlmErrorClass::ContextOverflow | LlmErrorClass::SessionCorruption => Duration::ZERO,
        LlmErrorClass::CallTimeout => Duration::ZERO,
    }
}

/// User-friendly message for each error class.
/// Language defaults to Russian when not specified.
pub fn user_message(class: &LlmErrorClass) -> &'static str {
    user_message_lang(class, "ru")
}

/// User-friendly message for each error class with explicit language.
pub fn user_message_lang(class: &LlmErrorClass, language: &str) -> &'static str {
    let e = super::localization::get_error_strings(language);
    match class {
        LlmErrorClass::ContextOverflow => e.context_overflow,
        LlmErrorClass::SessionCorruption => e.session_corruption,
        LlmErrorClass::TransientHttp => e.transient_http,
        LlmErrorClass::RateLimit => e.rate_limit,
        LlmErrorClass::AuthPermanent => e.auth_permanent,
        LlmErrorClass::Billing => e.billing,
        LlmErrorClass::Overloaded => e.overloaded,
        LlmErrorClass::CallTimeout => e.unknown,  // handled by retry UI, not error message
        LlmErrorClass::Unknown => e.unknown,
    }
}

/// Format error for user display: classify + user message with warning emoji.
/// Language defaults to Russian. Use `format_user_error_lang` for explicit language.
pub fn format_user_error(error: &anyhow::Error) -> String {
    format!("⚠️ {}", user_message(&classify(error)))
}

/// Format error for user display with explicit language.
#[allow(dead_code)]
pub fn format_user_error_lang(error: &anyhow::Error, language: &str) -> String {
    format!("⚠️ {}", user_message_lang(&classify(error), language))
}

/// Whether the error class is worth retrying at the engine level.
pub fn is_retryable(class: &LlmErrorClass) -> bool {
    matches!(
        class,
        LlmErrorClass::TransientHttp | LlmErrorClass::Overloaded | LlmErrorClass::RateLimit
    )
}

/// Asserts that CallTimeout is not included in the retryable set.
#[allow(dead_code)]
fn _assert_call_timeout_not_retryable() {
    // Compile-time assertion: if CallTimeout becomes retryable, this would need updating.
    let ct = LlmErrorClass::CallTimeout;
    assert!(!is_retryable(&ct), "CallTimeout must NOT be retryable by inner transient retry loop");
}

// ── RetryConfig ──────────────────────────────────────────────────────────────

/// Configuration for exponential backoff retry behaviour.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts (including the first).
    pub max_attempts: u32,
    /// Base delay for the first retry in milliseconds.
    pub base_delay_ms: u64,
    /// Maximum delay cap in milliseconds.
    pub max_delay_ms: u64,
    /// Fraction of the computed delay to add as random jitter (0.0–1.0).
    pub jitter_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay_ms: 500,
            max_delay_ms: 32_000,
            jitter_factor: 0.25,
        }
    }
}

impl RetryConfig {
    /// Compute the delay for a given attempt index (0-based).
    ///
    /// Formula: min(base * 2^attempt, max) + jitter * `delay_before_jitter`
    pub fn delay(&self, attempt: u32) -> Duration {
        use rand::Rng as _;
        let exp = (self.base_delay_ms as f64) * (2u64.pow(attempt) as f64);
        let capped = exp.min(self.max_delay_ms as f64) as u64;
        let jitter_ms = (rand::rng().random_range(0.0_f64..1.0) * self.jitter_factor * capped as f64) as u64;
        Duration::from_millis(capped + jitter_ms)
    }

    /// Return the recommended delay for a given error class and attempt.
    ///
    /// - `RateLimit` → full cooldown (60s) regardless of attempt
    /// - `TransientHttp` / Overloaded → exponential backoff
    /// - Others → exponential backoff as fallback
    pub fn retry_delay_for_error(&self, class: &LlmErrorClass, attempt: u32) -> Duration {
        match class {
            LlmErrorClass::RateLimit => cooldown_duration(class),
            _ => self.delay(attempt),
        }
    }
}

/// Extract a `Retry-After` value (seconds) embedded in an error message.
///
/// Providers embed the header value as `retry-after: N` when responding with
/// a 429 or 503. Returns `Some(Duration)` if the pattern is found.
pub fn extract_retry_after(error_msg: &str) -> Option<Duration> {
    static RE_RETRY_AFTER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)retry-after:\s*(\d+)").unwrap()
    });
    RE_RETRY_AFTER
        .captures(error_msg)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_overflow_patterns() {
        assert_eq!(classify_str("context length exceeded for model"), LlmErrorClass::ContextOverflow);
        assert_eq!(classify_str("error: token limit reached"), LlmErrorClass::ContextOverflow);
        assert_eq!(classify_str("request_too_large: prompt is too long"), LlmErrorClass::ContextOverflow);
        assert_eq!(classify_str("input too long for this model"), LlmErrorClass::ContextOverflow);
        assert_eq!(classify_str("exceeds the model maximum context"), LlmErrorClass::ContextOverflow);
    }

    #[test]
    fn session_corruption_patterns() {
        assert_eq!(classify_str("tool_use_block must follow assistant"), LlmErrorClass::SessionCorruption);
        assert_eq!(classify_str("roles must alternate between user and assistant"), LlmErrorClass::SessionCorruption);
        assert_eq!(classify_str("function call turn comes immediately after another"), LlmErrorClass::SessionCorruption);
        assert_eq!(classify_str("incorrect role information in messages"), LlmErrorClass::SessionCorruption);
    }

    #[test]
    fn transient_http_patterns() {
        assert_eq!(classify_str("minimax API error 502: bad gateway"), LlmErrorClass::TransientHttp);
        assert_eq!(classify_str("error 503 service unavailable"), LlmErrorClass::TransientHttp);
        assert_eq!(classify_str("HTTP 504 gateway timeout"), LlmErrorClass::TransientHttp);
        assert_eq!(classify_str("error sending request: 521"), LlmErrorClass::TransientHttp);
        assert_eq!(classify_str("minimax API error 500: internal server error"), LlmErrorClass::TransientHttp);
        assert_eq!(classify_str("HTTP 500 Internal Server Error"), LlmErrorClass::TransientHttp);
    }

    #[test]
    fn rate_limit_patterns() {
        assert_eq!(classify_str("API error 429: rate limit exceeded"), LlmErrorClass::RateLimit);
        assert_eq!(classify_str("too many requests, please slow down"), LlmErrorClass::RateLimit);
        assert_eq!(classify_str("tokens per minute limit reached"), LlmErrorClass::RateLimit);
        assert_eq!(classify_str("resource exhausted: TPM quota"), LlmErrorClass::RateLimit);
    }

    #[test]
    fn auth_patterns() {
        assert_eq!(classify_str("401 unauthorized: invalid api key"), LlmErrorClass::AuthPermanent);
        assert_eq!(classify_str("api key revoked or expired"), LlmErrorClass::AuthPermanent);
        assert_eq!(classify_str("403 forbidden: authentication failed"), LlmErrorClass::AuthPermanent);
    }

    #[test]
    fn billing_patterns() {
        assert_eq!(classify_str("HTTP 402 payment required"), LlmErrorClass::Billing);
        assert_eq!(classify_str("insufficient credits on account"), LlmErrorClass::Billing);
        assert_eq!(classify_str("quota exceeded for this month"), LlmErrorClass::Billing);
    }

    #[test]
    fn overloaded_patterns() {
        assert_eq!(classify_str("overloaded_error: server at capacity"), LlmErrorClass::Overloaded);
        assert_eq!(classify_str("service unavailable due to high demand"), LlmErrorClass::Overloaded);
    }

    #[test]
    fn google_permission_denied_is_auth() {
        assert_eq!(
            classify_str("google API error: PERMISSION_DENIED: API key not valid"),
            LlmErrorClass::AuthPermanent
        );
    }

    #[test]
    fn google_resource_exhausted_is_rate_limit() {
        assert_eq!(
            classify_str("google API error: RESOURCE_EXHAUSTED: GenerateContent request rate limit"),
            LlmErrorClass::RateLimit
        );
    }

    #[test]
    fn unknown_fallback() {
        assert_eq!(classify_str("something random happened"), LlmErrorClass::Unknown);
        assert_eq!(classify_str(""), LlmErrorClass::Unknown);
    }

    #[test]
    fn retryable_check() {
        assert!(is_retryable(&LlmErrorClass::TransientHttp));
        assert!(is_retryable(&LlmErrorClass::Overloaded));
        assert!(is_retryable(&LlmErrorClass::RateLimit));
        assert!(!is_retryable(&LlmErrorClass::AuthPermanent));
        assert!(!is_retryable(&LlmErrorClass::CallTimeout));
        assert!(!is_retryable(&LlmErrorClass::Unknown));
    }

    #[test]
    fn retry_config_defaults() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.max_attempts, 5);
        assert_eq!(cfg.base_delay_ms, 500);
        assert_eq!(cfg.max_delay_ms, 32_000);
        assert_eq!(cfg.jitter_factor, 0.25);
    }

    #[test]
    fn retry_config_delay_exponential() {
        let cfg = RetryConfig { jitter_factor: 0.0, ..RetryConfig::default() };
        // Without jitter the delay is deterministic.
        assert_eq!(cfg.delay(0).as_millis(), 500);
        assert_eq!(cfg.delay(1).as_millis(), 1000);
        assert_eq!(cfg.delay(2).as_millis(), 2000);
        assert_eq!(cfg.delay(3).as_millis(), 4000);
        assert_eq!(cfg.delay(4).as_millis(), 8000);
    }

    #[test]
    fn retry_config_delay_capped() {
        let cfg = RetryConfig { jitter_factor: 0.0, ..RetryConfig::default() };
        // Attempt 10 would be 512_000ms without cap — must be capped at 32_000.
        assert_eq!(cfg.delay(10).as_millis(), 32_000);
    }

    #[test]
    fn retry_delay_for_error_rate_limit_uses_cooldown() {
        let cfg = RetryConfig::default();
        // RateLimit should always return the full 60s cooldown, not exponential.
        let delay = cfg.retry_delay_for_error(&LlmErrorClass::RateLimit, 0);
        assert_eq!(delay.as_secs(), 60);
        let delay2 = cfg.retry_delay_for_error(&LlmErrorClass::RateLimit, 3);
        assert_eq!(delay2.as_secs(), 60);
    }

    #[test]
    fn retry_delay_for_error_transient_uses_backoff() {
        let cfg = RetryConfig { jitter_factor: 0.0, ..RetryConfig::default() };
        let delay = cfg.retry_delay_for_error(&LlmErrorClass::TransientHttp, 0);
        assert_eq!(delay.as_millis(), 500);
        let delay2 = cfg.retry_delay_for_error(&LlmErrorClass::TransientHttp, 2);
        assert_eq!(delay2.as_millis(), 2000);
    }

    #[test]
    fn extract_retry_after_found() {
        assert_eq!(
            extract_retry_after("OpenAI API error (retry-after: 30): rate limit exceeded"),
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(
            extract_retry_after("anthropic API error (Retry-After: 120): too many requests"),
            Some(std::time::Duration::from_secs(120))
        );
    }

    #[test]
    fn extract_retry_after_not_found() {
        assert_eq!(extract_retry_after("some random error without header"), None);
        assert_eq!(extract_retry_after(""), None);
    }

    #[test]
    fn billing_before_rate_limit() {
        // 402 should be billing, not confused with other patterns
        assert_eq!(classify_str("402 payment required"), LlmErrorClass::Billing);
    }

    #[test]
    fn classify_inactivity_timeout_is_call_timeout() {
        use crate::agent::providers::error::{LlmCallError, PartialState};
        let e = anyhow::Error::new(LlmCallError::InactivityTimeout {
            provider: "p".into(),
            silent_secs: 60,
            partial_state: PartialState::Empty,
        });
        assert_eq!(classify(&e), LlmErrorClass::CallTimeout);
    }

    #[test]
    fn classify_max_duration_exceeded_is_call_timeout() {
        use crate::agent::providers::error::{LlmCallError, PartialState};
        let e = anyhow::Error::new(LlmCallError::MaxDurationExceeded {
            provider: "p".into(),
            elapsed_secs: 600,
            partial_state: PartialState::Empty,
        });
        assert_eq!(classify(&e), LlmErrorClass::CallTimeout);
    }

    #[test]
    fn call_timeout_is_not_retryable() {
        assert!(!is_retryable(&LlmErrorClass::CallTimeout));
    }

    #[test]
    fn user_messages_not_empty() {
        let classes = [
            LlmErrorClass::ContextOverflow, LlmErrorClass::SessionCorruption,
            LlmErrorClass::TransientHttp, LlmErrorClass::RateLimit,
            LlmErrorClass::AuthPermanent, LlmErrorClass::Billing,
            LlmErrorClass::Overloaded, LlmErrorClass::CallTimeout, LlmErrorClass::Unknown,
        ];
        for class in &classes {
            assert!(!user_message(class).is_empty(), "empty message for {:?}", class);
        }
    }
}
