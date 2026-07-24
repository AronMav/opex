//! FSE allowlist: the hard-coded closed set of built-in `executor=tool`
//! actions that may ever 0-click auto-run, plus the caller-independent
//! validators reused by BOTH the HTTP write path and the agent `file_scenario`
//! tool. Modeled on the `VALID_CAPABILITIES` closed-set check in
//! `gateway/handlers/providers.rs:41,570`.
//!
//! Security-load-bearing: the constant cannot be extended at runtime. The
//! operator-editable "allowlist toggle" may only *disable* members of this
//! constant (see `validate_allowlist_toggle`), never admit a new name.

use std::fmt;

/// The five built-in deterministic action names. `save` is the rowless
/// universal fallback; it is listed here so dispatch + validation share one
/// closed set (design §4.2, §4.6).
#[allow(dead_code)] // Phase 5+: consumed by binding-write validator and HTTP route handlers
pub const FSE_DEFAULT_ALLOWLIST: &[&str] = &["transcribe", "describe", "extract_document", "save", "summarize_video"];

/// Reasons a binding write or allowlist amend is rejected.
#[allow(dead_code)] // Phase 5+: returned by validators, matched in HTTP handlers
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowlistError {
    /// An allowlist-toggle amend referenced a name absent from the constant.
    /// Maps to HTTP 400.
    UnknownMember(String),
}

impl fmt::Display for AllowlistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AllowlistError::UnknownMember(a) => write!(
                f,
                "'{a}' is not a member of FSE_DEFAULT_ALLOWLIST; allowed: {}",
                FSE_DEFAULT_ALLOWLIST.join(", ")
            ),
        }
    }
}

impl std::error::Error for AllowlistError {}

/// True if `name` is one of the four built-in action names (constant
/// membership, independent of the operator toggle).
#[allow(dead_code)] // private helper; used by the three pub validators below
fn is_constant_member(name: &str) -> bool {
    FSE_DEFAULT_ALLOWLIST.contains(&name)
}

/// Dispatch-time re-check for 0-click auto-run (design §4.6: "the constant is
/// re-checked before any auto-run, so even a forged DB row cannot 0-click-run a
/// non-built-in action"). Fail-closed: an empty toggle or a non-member yields
/// `false` and the caller must treat the action as unsupported.
///
/// Live via the handler-admin allowlist surface (`handlers_admin.rs`), which
/// uses it to decide which builtin handlers are auto-runnable. (The original
/// in-core FSE dispatcher that this was written for has been retired — the
/// toolgate-handler allowlist is the current consumer.)
pub fn is_allowed_for_autorun(action_ref: &str, enabled_allowlist: &[String]) -> bool {
    is_constant_member(action_ref) && enabled_allowlist.iter().any(|m| m == action_ref)
}

/// Closed-domain toggle validator: an allowlist amend may reference ONLY
/// members of `FSE_DEFAULT_ALLOWLIST`; any other name is rejected (design
/// §4.6, mirroring `providers.rs:570` `VALID_CAPABILITIES`). It can therefore
/// never admit `code_exec` / raw-URL / a YAML tool.
///
/// An empty slice is accepted and means all auto-run is operator-disabled.
// Live via set_enabled_allowlist (PUT /api/handlers/allowlist) + the shared allowlist surface.
pub fn validate_allowlist_toggle(members: &[String]) -> Result<(), AllowlistError> {
    for m in members {
        if !is_constant_member(m) {
            return Err(AllowlistError::UnknownMember(m.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Vec<String> {
        FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn constant_holds_exactly_the_five_builtins() {
        assert_eq!(FSE_DEFAULT_ALLOWLIST, &["transcribe", "describe", "extract_document", "save", "summarize_video"]);
    }

    #[test]
    fn autorun_recheck_is_fail_closed() {
        assert!(is_allowed_for_autorun("transcribe", &full()));
        assert!(!is_allowed_for_autorun("code_exec", &full()));
        assert!(!is_allowed_for_autorun("transcribe", &[])); // empty toggle => nothing auto-runs
    }

    #[test]
    fn toggle_rejects_non_constant_member() {
        let err = validate_allowlist_toggle(&["transcribe".into(), "code_exec".into()]).unwrap_err();
        assert!(matches!(err, AllowlistError::UnknownMember(ref a) if a == "code_exec"));
        assert!(validate_allowlist_toggle(&["transcribe".into(), "save".into()]).is_ok());
    }

    #[test]
    fn toggle_accepts_empty_slice() {
        // An empty allowlist is valid: it means the operator has disabled all
        // auto-run; no unknown members to reject.
        assert!(validate_allowlist_toggle(&[]).is_ok());
    }

    #[test]
    fn allowlist_contains_summarize_video() {
        assert!(FSE_DEFAULT_ALLOWLIST.contains(&"summarize_video"));
    }
}
