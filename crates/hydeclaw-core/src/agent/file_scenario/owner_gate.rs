//! Pure owner-gate predicate for FSE callback authorization (Task 9.7).
//!
//! This module is a leaf (only `anyhow` as a dep) so it can be mounted in the
//! lib facade (`lib.rs`) for integration-test access without dragging in the
//! full `file_scenario` module tree.

/// Pure owner-gate predicate for FSE callback runs.
///
/// Returns `Ok(())` when the caller is allowed to trigger a scenario run, or
/// an `Err` with a human-readable rejection reason when `is_owner` is `false`.
///
/// # Authorization semantics
///
/// - `is_owner = true`  → caller is the session owner (Telegram owner tap, or
///   a bearer-authenticated web call where ownership was confirmed upstream) → `Ok`
/// - `is_owner = false` → non-owner tap in a shared chat → `Err`
///
/// The Telegram `handle_fse_callback` in `gateway/handlers/channel_ws/inline.rs`
/// re-fetches the `is_owner` flag from the live `AccessGuard` and applies this
/// gate (line ~205 in inline.rs). The web `api_run_scenario` handler uses the
/// sibling `is_run_authorized(is_owner, channel_user_id)` predicate in
/// `gateway/handlers/file_scenarios/run.rs` for the dual web/channel path.
pub fn assert_fse_owner(is_owner: bool) -> anyhow::Result<()> {
    if is_owner {
        Ok(())
    } else {
        anyhow::bail!("not authorized: only the session owner may trigger file scenario runs")
    }
}
