//! CLI provider cooldown / circuit-breaker.
//!
//! [`CliErrorReason`] classifies stderr/stdout into a small enum and
//! [`CooldownState`] applies an exponential backoff per reason kind.
//! After the cooldown window expires the breaker resets (half-open →
//! closed) on the next successful call.

use std::time::{Duration, Instant};

/// Classified CLI error reason for cooldown decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum CliErrorReason {
    /// Rate limited (429, "too many requests", quota exceeded)
    RateLimit,
    /// Auth error (401/403, invalid key, revoked, banned)
    Auth,
    /// Billing issue (402, insufficient credits)
    Billing,
    /// Overloaded (503, "high demand")
    Overloaded,
    /// Timeout (process took too long)
    Timeout,
    /// Other/unknown error
    Unknown,
}

impl CliErrorReason {
    /// Cooldown duration for this error type (exponential: base * 5^(n-1), capped).
    fn cooldown_ms(&self, error_count: u32) -> u64 {
        let n = error_count.min(4);
        match self {
            // Rate limit / overload: 1m → 5m → 25m → 1h max
            CliErrorReason::RateLimit | CliErrorReason::Overloaded => {
                let ms = 60_000u64 * 5u64.pow(n.saturating_sub(1));
                ms.min(3_600_000) // 1 hour max
            }
            // Auth / billing: 5h → 10h → 20h → 24h max
            CliErrorReason::Auth | CliErrorReason::Billing => {
                let ms = 5 * 3_600_000u64 * 2u64.pow(n.saturating_sub(1));
                ms.min(24 * 3_600_000) // 24 hours max
            }
            // Timeout / unknown: 30s → 2m → 10m → 30m max
            CliErrorReason::Timeout | CliErrorReason::Unknown => {
                let ms = 30_000u64 * 5u64.pow(n.saturating_sub(1).min(3));
                ms.min(30 * 60_000) // 30 min max
            }
        }
    }
}

/// Classify an error from CLI output using shared `error_classify` module.
pub(super) fn classify_cli_error(stderr: &str, stdout: &str, _exit_code: i64) -> CliErrorReason {
    use crate::agent::error_classify::{LlmErrorClass, classify_str};
    let combined = format!("{stderr} {stdout}");
    match classify_str(&combined) {
        LlmErrorClass::RateLimit => CliErrorReason::RateLimit,
        LlmErrorClass::AuthPermanent => CliErrorReason::Auth,
        LlmErrorClass::Billing => CliErrorReason::Billing,
        LlmErrorClass::Overloaded | LlmErrorClass::TransientHttp => CliErrorReason::Overloaded,
        _ => CliErrorReason::Unknown,
    }
}

pub(super) struct CooldownState {
    /// Number of consecutive errors
    pub(super) error_count: u32,
    /// Cooldown expires at this instant
    pub(super) cooldown_until: Option<Instant>,
    /// Last error reason
    pub(super) last_reason: Option<CliErrorReason>,
}

impl CooldownState {
    pub(super) fn new() -> Self {
        Self { error_count: 0, cooldown_until: None, last_reason: None }
    }

    /// Check if currently in cooldown. If expired, reset state.
    pub(super) fn is_in_cooldown(&mut self) -> Option<Duration> {
        if let Some(until) = self.cooldown_until {
            let now = Instant::now();
            if now < until {
                return Some(until - now);
            }
            // Expired — reset (circuit breaker half-open → closed)
            self.error_count = 0;
            self.cooldown_until = None;
            self.last_reason = None;
        }
        None
    }

    /// Record a failure and start cooldown.
    pub(super) fn record_failure(&mut self, reason: CliErrorReason) {
        // Don't extend active cooldown window (OpenClaw pattern)
        if self.cooldown_until.is_some_and(|u| Instant::now() < u) {
            return;
        }
        self.error_count += 1;
        self.last_reason = Some(reason);
        let cooldown_ms = reason.cooldown_ms(self.error_count);
        self.cooldown_until = Some(Instant::now() + Duration::from_millis(cooldown_ms));
        tracing::warn!(
            reason = ?reason,
            error_count = self.error_count,
            cooldown_secs = cooldown_ms / 1000,
            "CLI provider entering cooldown"
        );
    }

    /// Record success — reset error count.
    pub(super) fn record_success(&mut self) {
        self.error_count = 0;
        self.cooldown_until = None;
        self.last_reason = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_after_failure() {
        let mut cd = CooldownState::new();
        assert!(cd.is_in_cooldown().is_none());
        cd.record_failure(CliErrorReason::RateLimit);
        assert!(cd.is_in_cooldown().is_some());
        assert_eq!(cd.error_count, 1);
    }

    #[test]
    fn cooldown_after_success_reset() {
        let mut cd = CooldownState::new();
        cd.record_failure(CliErrorReason::Unknown);
        // The "no extend during active cooldown" rule means a second
        // failure here would be a no-op — assert just one increment.
        assert_eq!(cd.error_count, 1);
        cd.record_success();
        assert_eq!(cd.error_count, 0);
        assert!(cd.is_in_cooldown().is_none());
    }

    #[test]
    fn cooldown_no_extend_during_active() {
        let mut cd = CooldownState::new();
        cd.record_failure(CliErrorReason::Auth);
        let until_first = cd.cooldown_until;
        cd.record_failure(CliErrorReason::Auth);
        // Second failure during active cooldown must not bump or extend.
        assert_eq!(cd.cooldown_until, until_first);
        assert_eq!(cd.error_count, 1);
    }

    #[test]
    fn cooldown_ms_rate_limit_escalation() {
        // 1m → 5m → 25m → 1h max
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(1), 60_000);
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(2), 300_000);
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(3), 1_500_000);
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(4), 3_600_000);
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(99), 3_600_000); // capped
    }

    #[test]
    fn cooldown_ms_unknown_escalation() {
        assert_eq!(CliErrorReason::Unknown.cooldown_ms(1), 30_000);
        assert_eq!(CliErrorReason::Unknown.cooldown_ms(2), 150_000);
        assert_eq!(CliErrorReason::Unknown.cooldown_ms(3), 750_000);
        assert_eq!(CliErrorReason::Unknown.cooldown_ms(4), 1_800_000); // capped at 30m
    }
}
