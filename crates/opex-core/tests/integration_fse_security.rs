//! FSE Phase 9.9: security guards — allowlist rejects `code_exec` at write-time
//! and dispatch fails closed (Unsupported) at dispatch-time.
//!
//! Two enforcement points:
//!
//! 1. **Write-time:** `validate_binding_write("tool", "code_exec", true, &enabled)`
//!    returns `Err(NotAllowlisted)`. Any write path (HTTP handler, agent tool)
//!    that respects this validator cannot store `code_exec` as a 0-click default.
//!
//! 2. **Dispatch-time (fail-closed backstop):** Even if a malicious or pre-existing
//!    row with `executor=tool, action_ref=code_exec, is_default=true` were to reach
//!    the dispatcher (bypassing the write-time validator), `dispatch_action` resolves
//!    `code_exec` via `resolve()` → `None` → `ScenarioOutcome::unsupported(...)`.
//!    The seam calls `run_builtin(b.action_ref)` which calls `dispatch_action` — so
//!    a forged row produces `Unsupported`, never invokes `code_exec`.
//!
//! No DB required for either test: `validate_binding_write` and `dispatch_action`
//! are pure functions. The `is_allowed_for_autorun` re-check guard is also tested
//! here as a third layer.

use opex_core::agent::file_scenario::dispatch::{dispatch_action, DispatchInput};
use opex_core::agent::file_scenario::ScenarioStatus;
use opex_core::agent::fse::allowlist::{
    is_allowed_for_autorun, validate_binding_write, AllowlistError, FSE_DEFAULT_ALLOWLIST,
};
use opex_core::opex_types::{MediaAttachment, MediaType};
use std::time::Duration;

// ── Helper ────────────────────────────────────────────────────────────────────

fn full_enabled() -> Vec<String> {
    FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect()
}

fn dummy_attachment() -> MediaAttachment {
    MediaAttachment {
        url: "https://pub.example/api/uploads/00000000-0000-0000-0000-000000000099?sig=x&exp=1"
            .into(),
        media_type: MediaType::Document,
        file_name: Some("evil.bin".into()),
        mime_type: Some("application/octet-stream".into()),
        file_size: None,
    }
}

// ── Test 1: FSE_DEFAULT_ALLOWLIST constant never contains code_exec ───────────

/// The hard-coded constant `FSE_DEFAULT_ALLOWLIST` must contain exactly the four
/// safe built-in deterministic actions and must never include `code_exec`,
/// `web_fetch`, `workspace_write`, or any YAML-tool name. This test is a
/// compile-time-equivalent assertion: if someone adds `code_exec` to the constant,
/// this test fails in CI before any other enforcement path is reached.
#[test]
fn allowlist_constant_excludes_code_exec_and_raw_fetch() {
    for forbidden in ["code_exec", "web_fetch", "workspace_write", "analyze_image"] {
        assert!(
            !FSE_DEFAULT_ALLOWLIST.contains(&forbidden),
            "{forbidden} must never be in the 0-click allowlist (FSE_DEFAULT_ALLOWLIST)"
        );
    }
    // Guard the exact set so additions require updating this assertion too.
    assert_eq!(
        FSE_DEFAULT_ALLOWLIST,
        &["transcribe", "describe", "extract_document", "save"],
        "FSE_DEFAULT_ALLOWLIST must be exactly the four safe built-in actions"
    );
}

// ── Test 2: write-time validator rejects code_exec as a 0-click default ───────

/// `validate_binding_write` is the caller-independent write-time gate used by
/// EVERY write path (HTTP `POST/PUT /api/file-scenarios`, `PUT .../{id}/default`,
/// and the agent `file_scenario` tool). It must reject `executor=tool` +
/// `is_default=true` with `action_ref="code_exec"` because `code_exec` is not
/// in `FSE_DEFAULT_ALLOWLIST`.
///
/// Also asserts that an allowlisted action (`transcribe`) is accepted, so the
/// validator cannot be trivially broken to reject everything.
#[test]
fn allowlist_rejects_code_exec_at_write() {
    let enabled = full_enabled();

    // ── Rejected: code_exec as a 0-click default ─────────────────────────────
    let res = validate_binding_write("tool", "code_exec", true, &enabled);
    assert!(
        res.is_err(),
        "validate_binding_write must reject code_exec as a 0-click default (executor=tool, is_default=true)"
    );
    let err = res.unwrap_err();
    assert!(
        matches!(err, AllowlistError::NotAllowlisted(ref a) if a == "code_exec"),
        "rejection must be NotAllowlisted(code_exec), got: {err:?}"
    );
    // Human-readable error must cite the allowlist.
    let msg = err.to_string();
    assert!(
        msg.contains("code_exec") && (msg.contains("allowlist") || msg.contains("allowed")),
        "rejection message must cite code_exec and the allowlist: {msg}"
    );

    // ── Accepted: an allowlisted action IS permitted as a 0-click default ────
    let ok = validate_binding_write("tool", "transcribe", true, &enabled);
    assert!(
        ok.is_ok(),
        "validate_binding_write must accept 'transcribe' as a 0-click default: {ok:?}"
    );

    // ── Exempted: executor=skill is never allowlist-gated ────────────────────
    let skill_ok = validate_binding_write("skill", "code_exec", true, &enabled);
    assert!(
        skill_ok.is_ok(),
        "executor=skill is never gated by the allowlist (only executor=tool is gated)"
    );

    // ── Exempted: is_default=false is never allowlist-gated ──────────────────
    let non_default_ok = validate_binding_write("tool", "code_exec", false, &enabled);
    assert!(
        non_default_ok.is_ok(),
        "is_default=false code_exec binding is not gated (only 0-click defaults are gated)"
    );
}

// ── Test 3: dispatch-time fail-closed — code_exec never executes ──────────────

/// Defense-in-depth: even if a `code_exec` `is_default=true` binding were to
/// reach the dispatcher (e.g. via a direct DB INSERT that bypassed the write
/// validator, or a stale row from a previous deployment), `dispatch_action`
/// resolves `code_exec` via the closed `resolve()` table → `None` →
/// `ScenarioOutcome::unsupported(...)`. The seam calls `run_builtin(b.action_ref)`
/// which is exactly `dispatch_action(DispatchInput { action_ref, ... })`, so the
/// forged row's `action_ref` is NEVER executed — it yields `Unsupported`.
///
/// The `toolgate_url` points to a port where nothing listens (`127.0.0.1:1`).
/// If `dispatch_action` were to call toolgate for `code_exec`, the test would
/// hang or fail with a connection error. The fact that it returns immediately with
/// `Unsupported` proves toolgate was never contacted.
#[tokio::test]
async fn dispatch_fails_closed_on_forged_binding() {
    let client = reqwest::Client::new();
    let att = dummy_attachment();

    // Simulate the seam calling run_builtin("code_exec") after finding a
    // forged is_default=true row in the DB. The toolgate URL is unreachable;
    // if dispatch_action tried to call it, this test would error, not pass.
    let outcome = dispatch_action(DispatchInput {
        action_ref: "code_exec",
        attachment: &att,
        toolgate_url: "http://127.0.0.1:1",       // unreachable — proof it is never called
        gateway_listen: "0.0.0.0:18789",
        language: "en",
        http_client: &client,
        timeout: Duration::from_secs(5),
        enqueue: None,
    })
    .await;

    assert_eq!(
        outcome.status,
        ScenarioStatus::Unsupported,
        "dispatch_action must return Unsupported for code_exec — never execute it: {:?}",
        outcome.reason
    );
    assert!(
        outcome.artifact_urls.is_empty(),
        "forged code_exec dispatch must produce no artifacts: {:?}",
        outcome.artifact_urls
    );
    let reason = outcome.reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("code_exec"),
        "Unsupported reason must cite code_exec: {reason:?}"
    );
}

// ── Test 4: is_allowed_for_autorun — dispatch-time re-check fail-closed ───────

/// `is_allowed_for_autorun` is the third enforcement layer: a dispatch-time
/// re-check of the constant that runs before ANY 0-click auto-run, guarding
/// against stale operator-toggle state (e.g. the DB toggle was updated but the
/// in-memory cache hasn't refreshed yet). Fail-closed: returns `false` for
/// `code_exec` regardless of the enabled-allowlist contents.
///
/// This is the §4.6 re-check gate. The `dispatch_seam.rs` integration tests
/// that use a live DB verify it through `dispatch_attachments`; this test
/// exercises the predicate in isolation.
#[test]
fn is_allowed_for_autorun_rejects_code_exec_fail_closed() {
    let enabled = full_enabled();

    // code_exec is never in the constant → always false.
    assert!(
        !is_allowed_for_autorun("code_exec", &enabled),
        "is_allowed_for_autorun must return false for code_exec (not a constant member)"
    );

    // Applies even when the enabled list is the full constant — it's still not a member.
    assert!(
        !is_allowed_for_autorun("code_exec", &[
            "transcribe".into(), "describe".into(), "extract_document".into(), "save".into()
        ]),
        "is_allowed_for_autorun must return false for code_exec even when all 4 builtins are enabled"
    );

    // An empty toggle (operator disabled all auto-run) also returns false.
    assert!(
        !is_allowed_for_autorun("transcribe", &[]),
        "is_allowed_for_autorun must return false for any action when the toggle is empty"
    );

    // A valid constant member IS allowed when it's in the enabled list.
    assert!(
        is_allowed_for_autorun("transcribe", &enabled),
        "is_allowed_for_autorun must return true for 'transcribe' when it's enabled"
    );
}

// ── Test 5: dispatch table resolve — code_exec has no entry ──────────────────

/// White-box guard on `resolve()`: the closed dispatch table must map `code_exec`
/// to `None`. This is the mechanism that makes `dispatch_action` fail-closed
/// without touching any allowlist logic — the table simply has no entry for it.
/// A future contributor cannot silently add `code_exec` to the table without
/// this test failing.
#[test]
fn dispatch_resolve_code_exec_is_none() {
    use opex_core::agent::file_scenario::dispatch::resolve;

    assert_eq!(
        resolve("code_exec"),
        None,
        "resolve(code_exec) must return None — the dispatch table is closed against code_exec"
    );
    // Other dangerous tool names must also be absent.
    for name in ["workspace_write", "workspace_delete", "process_start", "web_fetch", "analyze_image"] {
        assert_eq!(
            resolve(name),
            None,
            "resolve({name}) must return None — only the 4 built-ins are in the dispatch table"
        );
    }
    // The four allowed built-ins must resolve.
    for name in ["transcribe", "describe", "extract_document", "save"] {
        assert!(
            resolve(name).is_some(),
            "resolve({name}) must return Some — it is a built-in deterministic action"
        );
    }
}
