//! Classification of how a session is being entered: brand-new, continuation
//! after a clean `done`, recovery into a still-`running` session (mid-task
//! crash / restart), or an explicit user resume from the UI.
//!
//! Used by bootstrap to decide whether to warm `LoopDetector` from the timeline and
//! by `claim_session_for_reentry` to decide which status transitions are legal.

use crate::SessionStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReentryMode {
    /// Session row was just created — no prior history.
    NewSession,
    /// Re-entering a session that completed cleanly (`run_status = 'done'`).
    /// New user turn appended to existing chain.
    NewTurnAfterDone,
    /// Re-entering a session whose previous run was still `'running'` —
    /// either the process crashed mid-loop or another worker is racing us.
    /// LoopDetector should warm from timeline to preserve error streaks.
    ResumeRunning,
    /// User explicitly opened a session via UI deep-link, fork, or
    /// `resume_session_id`. Status may be anything (including soft-terminal
    /// like `failed`); the user is consciously continuing, so
    /// `claim_session_for_reentry` allows the transition unconditionally.
    /// Like `ResumeRunning`, this warms the `LoopDetector`.
    ExplicitResume,
}

impl ReentryMode {
    /// Should bootstrap warm the `LoopDetector` from past timeline tool events?
    /// True for `ResumeRunning` and `ExplicitResume` — both indicate the user
    /// is continuing a previous run and prior error streaks remain relevant.
    /// False for `NewSession` / `NewTurnAfterDone` — fresh turn, past errors
    /// don't apply.
    pub fn warm_loop_detector(self) -> bool {
        matches!(self, Self::ResumeRunning | Self::ExplicitResume)
    }

    /// Classify based on the existing `run_status` value at lookup time.
    /// `None` means the row is freshly inserted (no run_status set yet).
    ///
    /// Soft-terminal statuses (Failed/Interrupted/Timeout/Cancelled) should
    /// never reach here via `resolve_active_dm_session` (which filters them).
    /// If one slips through (test, future caller), we warn and fall back to
    /// `NewSession` rather than panic — process-wide panics for a recoverable
    /// classification bug would be too disruptive in production.
    pub fn classify(prior: Option<SessionStatus>) -> Self {
        match prior {
            None => Self::NewSession,
            Some(SessionStatus::Running) => Self::ResumeRunning,
            Some(SessionStatus::Done) => Self::NewTurnAfterDone,
            Some(other) => {
                tracing::warn!(
                    status = ?other,
                    "ReentryMode::classify on non-done terminal — defaulting to NewSession; \
                     resolve_active_dm_session should have filtered this row",
                );
                Self::NewSession
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_none_is_new() {
        assert_eq!(ReentryMode::classify(None), ReentryMode::NewSession);
    }

    #[test]
    fn classify_running_is_resume() {
        assert_eq!(
            ReentryMode::classify(Some(SessionStatus::Running)),
            ReentryMode::ResumeRunning,
        );
    }

    #[test]
    fn classify_done_is_new_turn() {
        assert_eq!(
            ReentryMode::classify(Some(SessionStatus::Done)),
            ReentryMode::NewTurnAfterDone,
        );
    }

    #[test]
    fn classify_failed_falls_back_to_new() {
        assert_eq!(
            ReentryMode::classify(Some(SessionStatus::Failed)),
            ReentryMode::NewSession,
        );
        assert_eq!(
            ReentryMode::classify(Some(SessionStatus::Interrupted)),
            ReentryMode::NewSession,
        );
        assert_eq!(
            ReentryMode::classify(Some(SessionStatus::Timeout)),
            ReentryMode::NewSession,
        );
        assert_eq!(
            ReentryMode::classify(Some(SessionStatus::Cancelled)),
            ReentryMode::NewSession,
        );
    }

    #[test]
    fn warm_loop_detector_only_for_resume_or_explicit() {
        assert!(!ReentryMode::NewSession.warm_loop_detector());
        assert!(!ReentryMode::NewTurnAfterDone.warm_loop_detector());
        assert!(ReentryMode::ResumeRunning.warm_loop_detector());
        assert!(ReentryMode::ExplicitResume.warm_loop_detector());
    }
}
