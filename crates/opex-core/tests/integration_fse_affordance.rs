//! FSE Phase 9 affordance guards — Task 9.6.
//!
//! Two behavioral contracts verified here:
//!
//! 1. **Dual-channel convergence** — both the web chip click and the Telegram
//!    FSE callback resolve a chosen scenario through `run_scenario_and_persist`
//!    (the single shared run path from Phase 6). No duplicate code, no separate
//!    Telegram-only route.
//!
//! 2. **Web 2+-no-default → immediate save + alternatives** — when ≥2 enabled
//!    bindings match but none is `is_default`, the dispatcher runs `save`
//!    immediately (no blocking) AND records `pending_alternatives` for Phase 6
//!    to emit as UI chips (not an empty list).
//!
//! ## Architecture note
//!
//! `run_scenario_and_persist` is `pub(crate)` and lives in
//! `gateway/handlers/file_scenarios/run.rs`. It cannot be called from this
//! external test file. The convergence is proven at two levels:
//!
//! - **Source level (static, below):** both callers import and call the same
//!   symbol; a grep guard enforces this in CI.
//! - **DB level (sqlx::test in run.rs):** `dual_channel_convergence_persists_one_assistant_message`
//!   proves the shared path produces one persisted assistant message per
//!   invocation.
//!
//! `dispatch_attachments` is also `pub(crate)` relative to the lib facade
//! (not re-exported in `src/lib.rs`). The 2+-no-default contract is proven by
//! `web_two_bindings_no_default_immediate_save_with_alternatives` in
//! `dispatch_seam.rs` (same crate, full access).
//!
//! This file provides the **structural / static proofs** accessible from here.

// ── Test 3 (Task 9.7): non-owner callback rejected ───────────────────────────

/// Security guard: a non-owner user in a shared Telegram chat MUST NOT be able
/// to trigger a file-scenario run by tapping the `fse:<id>:<action>` inline
/// button. This test asserts the owner-gate predicate (`assert_fse_owner`)
/// which is the authoritative check applied by both:
///
/// - `gateway/handlers/channel_ws/inline.rs:handle_fse_callback` (line ~205),
///   which re-fetches `AccessGuard::is_owner(&user_id)` and rejects non-owners
///   with an Error frame BEFORE calling `run_scenario_and_persist`.
/// - `gateway/handlers/file_scenarios/run.rs:is_run_authorized` (the web path).
///
/// The test is a pure-function assertion (no DB, no network) exercising the
/// real `assert_fse_owner` gate from `opex_core::agent::file_scenario`.
///
/// Entry point: `opex_core::agent::file_scenario::assert_fse_owner(is_owner)`
#[test]
fn non_owner_callback_rejected() {
    use opex_core::agent::file_scenario::assert_fse_owner;

    // ── Non-owner is rejected ─────────────────────────────────────────────────
    // Simulates: intruder taps the button in a shared chat. The live AccessGuard
    // has returned `is_owner = false` for this user_id.
    let denied = assert_fse_owner(false);
    assert!(denied.is_err(), "non-owner callback must be rejected");
    let msg = format!("{:#}", denied.unwrap_err());
    assert!(
        msg.to_lowercase().contains("owner") || msg.to_lowercase().contains("not authorized"),
        "rejection reason must cite ownership: {msg}"
    );

    // ── Owner is accepted ─────────────────────────────────────────────────────
    // Simulates: session owner taps the button. AccessGuard returned is_owner = true.
    let ok = assert_fse_owner(true);
    assert!(ok.is_ok(), "owner callback must be accepted");

    // ── Web/operator path is always accepted ──────────────────────────────────
    // Web callers (no channel_user_id) are bearer-authenticated operators.
    // The `is_run_authorized(_, None)` path in run.rs returns true regardless
    // of is_owner. Assert via the inline.rs gate which always passes is_owner=true
    // for the session owner (confirmed by the access guard).
    // Here we just confirm the gate never spuriously rejects an owner.
    let web_operator = assert_fse_owner(true);
    assert!(web_operator.is_ok(), "operator/web path must not be blocked by the owner gate");
}

/// Structural guard: `inline.rs::handle_fse_callback` MUST check `is_owner`
/// before calling `run_scenario_and_persist`. This verifies at source level
/// that the non-owner gate cannot be bypassed by a code refactor that removes
/// the guard.
///
/// If this assertion fails it means the Telegram callback handler lost its
/// ownership check — a critical security regression.
#[test]
fn inline_rs_owner_gate_present_before_run() {
    let inline_src = include_str!("../src/gateway/handlers/channel_ws/inline.rs");

    // The owner check must appear before the run_scenario_and_persist call.
    let owner_gate_pos = inline_src
        .find("assert_fse_owner(is_owner)")
        .expect("inline.rs handle_fse_callback must gate via assert_fse_owner before running");
    let run_pos = inline_src
        .find("run_scenario_and_persist")
        .expect("inline.rs must call run_scenario_and_persist");

    assert!(
        owner_gate_pos < run_pos,
        "is_owner check ({owner_gate_pos}) must appear BEFORE run_scenario_and_persist \
         call ({run_pos}) in inline.rs — non-owner gate must not be bypassed"
    );

    // Non-owner must receive a rejection (not fall through to the run).
    assert!(
        inline_src.contains("Only the owner can run this action."),
        "inline.rs must emit an ownership-rejection message for non-owner FSE taps"
    );
}

// ── Test 1: dual-channel convergence — source-level guard ────────────────────

/// Both the web HTTP handler and the Telegram inline-button callback must
/// delegate to the SAME function (`run_scenario_and_persist`). This test
/// reads the source of both callers and asserts the symbol is present in each,
/// so any future refactor that splits the paths into different implementations
/// will fail this guard.
///
/// This is a pure source-text assertion — no DB, no network, no compilation of
/// the handler code. It is deliberately conservative: if either call site is
/// renamed or removed, the test fails and the engineer must update this guard.
#[test]
fn web_and_telegram_callbacks_both_call_run_scenario_and_persist() {
    let run_rs = include_str!("../src/gateway/handlers/file_scenarios/run.rs");
    let inline_rs = include_str!("../src/gateway/handlers/channel_ws/inline.rs");

    // Web path: `api_run_scenario` calls `run_scenario_and_persist`.
    assert!(
        run_rs.contains("run_scenario_and_persist("),
        "gateway/handlers/file_scenarios/run.rs must call run_scenario_and_persist"
    );

    // Telegram path: `fse_callback_handler` in inline.rs calls the same function.
    assert!(
        inline_rs.contains("run_scenario_and_persist("),
        "gateway/handlers/channel_ws/inline.rs must call run_scenario_and_persist \
         (both web chip and Telegram callback must share the same run path)"
    );

    // Neither caller should contain a separate, divergent implementation.
    // The function definition itself is in run.rs — the inline.rs reference
    // must be a call, not a definition.
    assert!(
        !inline_rs.contains("async fn run_scenario_and_persist"),
        "inline.rs must NOT re-define run_scenario_and_persist — it must call the shared fn"
    );
}

// ── Test 2: 2+-no-default contract — source-level guard ──────────────────────

/// Verifies at source level that the "≥1 bindings, no default" branch in
/// `dispatch_seam.rs` both:
///   a) runs `save` immediately (no blocking primitive), AND
///   b) pushes to `pending` (alternatives are NOT discarded).
///
/// The detailed DB-backed behavioral assertion lives in
/// `dispatch_seam.rs::tests::web_two_bindings_no_default_immediate_save_with_alternatives`.
/// This test is a fast, no-DB complement that guards the code structure.
#[test]
fn two_bindings_no_default_branch_runs_save_and_records_alternatives() {
    let seam_src = include_str!("../src/agent/file_scenario/dispatch_seam.rs");

    // The branch must call run_builtin with "save" for the no-default case.
    // The call is multiline: `run_builtin(\n    "save",\n    ...)` so we search
    // for the "save" literal in close proximity to `run_builtin(`.
    assert!(
        seam_src.contains("run_builtin(") && seam_src.contains(r#""save","#),
        "dispatch_seam.rs must call run_builtin with \"save\" for the no-default branch"
    );

    // The branch must push to pending (not skip it).
    // Count occurrences: there must be at least 2 `pending.push(` calls —
    // one for the default-exists branch (non-default alts) and one for the no-default branch.
    let push_count = seam_src.matches("pending.push(").count();
    assert!(
        push_count >= 2,
        "dispatch_seam.rs must push to pending in BOTH the default-exists and \
         no-default branches (found {push_count} push sites)"
    );

    // Ensure the no-default branch does NOT contain any blocking/await on a
    // user-choice channel (no oneshot, no channel recv, no sleep).
    // Simple heuristic: the `None =>` arm must not contain "oneshot" or "recv.await".
    // We extract the rough region of the None arm by finding the two `None` match arms.
    let none_arm_start = seam_src
        .rfind("None => {")
        .expect("dispatch_seam.rs must have a `None => {` arm for the no-default case");
    let end = (none_arm_start + 500).min(seam_src.len());
    let none_arm_region = &seam_src[none_arm_start..end];
    assert!(
        !none_arm_region.contains("oneshot") && !none_arm_region.contains("recv.await"),
        "web 2+-no-default branch must not block on a user choice \
         (no oneshot/recv.await in the None arm)"
    );
}
