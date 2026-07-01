//! FSE allowlist security guards (surviving after legacy retirement).
//! `FSE_DEFAULT_ALLOWLIST` never contains dangerous tools, and the dispatch-time
//! `is_allowed_for_autorun` re-check fails closed. Both are shared with the
//! File Handler Hub's builtin gating.

use opex_core::agent::fse::allowlist::{is_allowed_for_autorun, FSE_DEFAULT_ALLOWLIST};

#[test]
fn allowlist_constant_excludes_code_exec_and_raw_fetch() {
    for forbidden in ["code_exec", "web_fetch", "workspace_write", "analyze_image"] {
        assert!(
            !FSE_DEFAULT_ALLOWLIST.contains(&forbidden),
            "{forbidden} must never be in the 0-click allowlist (FSE_DEFAULT_ALLOWLIST)"
        );
    }
    assert_eq!(
        FSE_DEFAULT_ALLOWLIST,
        &["transcribe", "describe", "extract_document", "save", "summarize_video"],
        "FSE_DEFAULT_ALLOWLIST must be exactly the five safe built-in actions"
    );
}

#[test]
fn is_allowed_for_autorun_rejects_code_exec_fail_closed() {
    let enabled: Vec<String> = FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect();
    assert!(!is_allowed_for_autorun("code_exec", &enabled));
    assert!(!is_allowed_for_autorun("transcribe", &[]));
    assert!(is_allowed_for_autorun("transcribe", &enabled));
}
