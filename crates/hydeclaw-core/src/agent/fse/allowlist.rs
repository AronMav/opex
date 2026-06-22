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

/// The four built-in deterministic action names. `save` is the rowless
/// universal fallback; it is listed here so dispatch + validation share one
/// closed set (design §4.2, §4.6).
pub const FSE_DEFAULT_ALLOWLIST: &[&str] = &["transcribe", "describe", "extract_document", "save"];

/// Reasons a binding write or allowlist amend is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowlistError {
    /// `executor=tool` + `is_default=true` whose `action_ref` is not an
    /// enabled member of `FSE_DEFAULT_ALLOWLIST`. Maps to HTTP 400.
    NotAllowlisted(String),
    /// An allowlist-toggle amend referenced a name absent from the constant.
    /// Maps to HTTP 400.
    UnknownMember(String),
}

impl fmt::Display for AllowlistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AllowlistError::NotAllowlisted(a) => write!(
                f,
                "action '{a}' is not an allowlisted 0-click default; allowed: {}",
                FSE_DEFAULT_ALLOWLIST.join(", ")
            ),
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
fn is_constant_member(name: &str) -> bool {
    FSE_DEFAULT_ALLOWLIST.contains(&name)
}

/// Caller-independent write-time validator. Called by EVERY write path
/// (HTTP `POST/PUT /api/file-scenarios`, `PUT .../{id}/default`, and the
/// agent `file_scenario` tool). Only `executor=tool` + `is_default=true`
/// rows are gated; `executor=skill` and non-default rows are never gated by
/// the allowlist (they still pass `needs_approval`/deny/SSRF downstream).
///
/// `enabled_allowlist` is the operator-editable subset of the constant
/// (from `get_enabled_allowlist`). A default binding is accepted only if its
/// `action_ref` is BOTH a constant member AND currently enabled.
pub fn validate_binding_write(
    executor: &str,
    action_ref: &str,
    is_default: bool,
    enabled_allowlist: &[String],
) -> Result<(), AllowlistError> {
    if executor != "tool" || !is_default {
        return Ok(());
    }
    if is_constant_member(action_ref)
        && enabled_allowlist.iter().any(|m| m == action_ref)
    {
        Ok(())
    } else {
        Err(AllowlistError::NotAllowlisted(action_ref.to_string()))
    }
}

/// Dispatch-time re-check, run before ANY 0-click auto-run (design §4.6:
/// "the constant is re-checked before any auto-run, so even a forged DB row
/// cannot 0-click-run a non-built-in action"). Fail-closed: an empty toggle
/// or a non-member yields `false` and the dispatcher must resolve to
/// `ScenarioStatus::Unsupported`.
pub fn is_allowed_for_autorun(action_ref: &str, enabled_allowlist: &[String]) -> bool {
    is_constant_member(action_ref) && enabled_allowlist.iter().any(|m| m == action_ref)
}

/// Closed-domain toggle validator: an allowlist amend may reference ONLY
/// members of `FSE_DEFAULT_ALLOWLIST`; any other name is rejected (design
/// §4.6, mirroring `providers.rs:570` `VALID_CAPABILITIES`). It can therefore
/// never admit `code_exec` / raw-URL / a YAML tool.
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
    fn constant_holds_exactly_the_four_builtins() {
        assert_eq!(FSE_DEFAULT_ALLOWLIST, &["transcribe", "describe", "extract_document", "save"]);
    }

    #[test]
    fn rejects_default_tool_outside_constant() {
        let err = validate_binding_write("tool", "code_exec", true, &full()).unwrap_err();
        assert!(matches!(err, AllowlistError::NotAllowlisted(ref a) if a == "code_exec"));
    }

    #[test]
    fn accepts_default_tool_in_constant() {
        assert!(validate_binding_write("tool", "transcribe", true, &full()).is_ok());
    }

    #[test]
    fn rejects_member_disabled_in_toggle() {
        // "describe" is a constant member but operator disabled it → still rejected as a 0-click default.
        let enabled = vec!["transcribe".to_string(), "extract_document".to_string(), "save".to_string()];
        let err = validate_binding_write("tool", "describe", true, &enabled).unwrap_err();
        assert!(matches!(err, AllowlistError::NotAllowlisted(_)));
    }

    #[test]
    fn ignores_skill_executor_and_non_default() {
        // executor=skill is never allowlist-gated; is_default=false tool is also fine.
        assert!(validate_binding_write("skill", "my_recipe", true, &full()).is_ok());
        assert!(validate_binding_write("tool", "code_exec", false, &full()).is_ok());
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
}
