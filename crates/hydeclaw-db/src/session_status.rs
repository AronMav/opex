/// Session lifecycle FSM. Enforces valid state transitions at the Rust level.
/// The SQL layer (`set_session_run_status`) provides the hard DB-level guard;
/// this enum provides early detection of logic errors in tests and code review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Done,
    Failed,
    Interrupted,
    Timeout,
    Cancelled,
}

impl SessionStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// FSM transition rules:
    /// - `done → anything`: false (done is the only immutable terminal)
    /// - `running → anything`: true
    /// - `soft-terminal → running`: true (session re-entry after interrupted/failed/etc.)
    /// - `soft-terminal → soft-terminal`: false (cannot jump between terminal states)
    pub fn can_transition_to(self, to: Self) -> bool {
        match self {
            Self::Done => false,
            Self::Running => true,
            _ => to == Self::Running,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "interrupted" => Some(Self::Interrupted),
            "timeout" => Some(Self::Timeout),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_cannot_transition_to_anything() {
        use SessionStatus::*;
        for to in [Running, Done, Failed, Interrupted, Timeout, Cancelled] {
            assert!(
                !Done.can_transition_to(to),
                "Done should not transition to {:?}",
                to
            );
        }
    }

    #[test]
    fn running_can_transition_to_anything() {
        use SessionStatus::*;
        for to in [Running, Done, Failed, Interrupted, Timeout, Cancelled] {
            assert!(
                Running.can_transition_to(to),
                "Running should be able to transition to {:?}",
                to
            );
        }
    }

    #[test]
    fn soft_terminal_can_only_transition_to_running() {
        use SessionStatus::*;
        let soft_terminals = [Failed, Interrupted, Timeout, Cancelled];
        for from in soft_terminals {
            assert!(
                from.can_transition_to(Running),
                "{:?} should be able to transition to Running",
                from
            );
            for to in [Done, Failed, Interrupted, Timeout, Cancelled] {
                assert!(
                    !from.can_transition_to(to),
                    "{:?} should not transition to {:?}",
                    from,
                    to
                );
            }
        }
    }

    #[test]
    fn as_str_and_from_str_round_trip() {
        use SessionStatus::*;
        for status in [Running, Done, Failed, Interrupted, Timeout, Cancelled] {
            let s = status.as_str();
            assert_eq!(SessionStatus::from_str(s), Some(status), "round-trip failed for {:?}", status);
        }
    }

    #[test]
    fn from_str_unknown_returns_none() {
        assert_eq!(SessionStatus::from_str("unknown"), None);
        assert_eq!(SessionStatus::from_str(""), None);
    }

    #[test]
    fn is_terminal_only_running_is_non_terminal() {
        use SessionStatus::*;
        assert!(!Running.is_terminal());
        for s in [Done, Failed, Interrupted, Timeout, Cancelled] {
            assert!(s.is_terminal(), "{:?} should be terminal", s);
        }
    }
}
