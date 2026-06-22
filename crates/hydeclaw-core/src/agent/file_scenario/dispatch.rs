//! In-core dispatch table for built-in deterministic FSE actions.
//!
//! Fail-closed (security-load-bearing): an `executor=tool` `action_ref` not
//! present in this table resolves to `None` and the caller emits
//! `ScenarioOutcome{status: Unsupported}`. It NEVER falls through to a YAML
//! tool or a generic executor. A future contributor must not add a generic
//! fallthrough arm.

/// The built-in deterministic action names that the dispatch table resolves.
/// 1:1 with [`crate::agent::file_scenario::outcome::FSE_DEFAULT_ALLOWLIST`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinAction {
    Transcribe,
    Describe,
    ExtractDocument,
    Save,
}

/// Fail-closed resolution of an `action_ref` to a built-in. Returns `None` for
/// anything outside the closed set — the caller turns `None` into
/// `ScenarioOutcome::unsupported(...)`. NO generic fallthrough.
pub fn resolve(action_ref: &str) -> Option<BuiltinAction> {
    match action_ref {
        "transcribe" => Some(BuiltinAction::Transcribe),
        "describe" => Some(BuiltinAction::Describe),
        "extract_document" => Some(BuiltinAction::ExtractDocument),
        "save" => Some(BuiltinAction::Save),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_builtins() {
        assert_eq!(resolve("transcribe"), Some(BuiltinAction::Transcribe));
        assert_eq!(resolve("describe"), Some(BuiltinAction::Describe));
        assert_eq!(resolve("extract_document"), Some(BuiltinAction::ExtractDocument));
        assert_eq!(resolve("save"), Some(BuiltinAction::Save));
    }

    #[test]
    fn resolve_unknown_is_none_fail_closed() {
        // A stray / forged allowlist member or binding row must be inert.
        assert_eq!(resolve("code_exec"), None);
        assert_eq!(resolve("analyze_image"), None); // YAML tool name, not an action name
        assert_eq!(resolve(""), None);
        assert_eq!(resolve("Transcribe"), None); // case-sensitive
    }

    #[test]
    fn every_allowlist_member_resolves() {
        for name in crate::agent::file_scenario::outcome::FSE_DEFAULT_ALLOWLIST {
            assert!(resolve(name).is_some(), "allowlist member {name} must resolve to a builtin");
        }
    }
}
