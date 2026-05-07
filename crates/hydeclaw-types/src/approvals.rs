//! ApprovalAction enum for SSE wire/StreamEvent boundary.
//!
//! Wire format invariant: `#[serde(rename_all = "snake_case")]` produces
//! `"approved" | "rejected" | "timeout_rejected"`. ts-rs derives the
//! corresponding TS string-literal union. Three variants only — DB layer
//! `pending_approvals.status` (which carries a 4th `"pending"` value) is
//! a separate concern and uses raw `String` for its lifecycle state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAction {
    Approved,
    Rejected,
    TimeoutRejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_action_wire_format() {
        assert_eq!(serde_json::to_string(&ApprovalAction::Approved).unwrap(), "\"approved\"");
        assert_eq!(serde_json::to_string(&ApprovalAction::Rejected).unwrap(), "\"rejected\"");
        assert_eq!(
            serde_json::to_string(&ApprovalAction::TimeoutRejected).unwrap(),
            "\"timeout_rejected\""
        );
    }

    #[test]
    fn approval_action_roundtrip() {
        for a in [
            ApprovalAction::Approved,
            ApprovalAction::Rejected,
            ApprovalAction::TimeoutRejected,
        ] {
            let s = serde_json::to_string(&a).unwrap();
            let back: ApprovalAction = serde_json::from_str(&s).unwrap();
            assert_eq!(a, back);
        }
    }
}
