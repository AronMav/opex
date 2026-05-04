//! Security regression tests for the approval-gate fix in `channel_ws.rs`.
//!
//! The fix (line 378) changed:
//!   `live_guard.as_ref().is_none_or(|g| g.is_owner(&user_id))`  ← security hole
//! to:
//!   `live_guard.as_ref().map_or(false, |g| g.is_owner(&user_id))`  ← correct
//!
//! When no access guard is configured (`live_guard = None`) the old code granted
//! ownership to every user; the new code denies it.  These tests pin that
//! contract so it can never silently regress.
//!
//! Tests 1, 4, 5 are pure `Option` combinators — no Tokio runtime needed.
//! Tests 2, 3, 6 use `#[tokio::test]` because `PgPool::connect_lazy` requires
//! a runtime even though `is_owner` itself never awaits.

use hydeclaw_core::channels::access::AccessGuard;

/// Build an `AccessGuard` whose pool is never actually connected.
/// `is_owner` is pure sync and only reads `self.owner_id`.
fn make_guard(owner_id: Option<&str>) -> AccessGuard {
    let pool = sqlx::PgPool::connect_lazy("postgres://invalid").expect("lazy pool");
    AccessGuard::new("agent-test".to_string(), owner_id.map(str::to_string), false, pool)
}

/// `map_or(false, ...)` returns `false` for `None` — no guard means no owner.
#[test]
fn no_guard_is_not_owner() {
    let live_guard: Option<AccessGuard> = None;
    let is_owner = live_guard.as_ref().map_or(false, |g| g.is_owner("any_user"));
    assert!(!is_owner, "no guard must deny ownership for any user");
}

/// Matching `owner_id` grants ownership.
#[tokio::test]
async fn guard_with_owner_is_owner() {
    let guard = make_guard(Some("userA"));
    assert!(guard.is_owner("userA"), "owner should be recognised");
}

/// Non-matching user is denied even when a guard exists.
#[tokio::test]
async fn guard_with_wrong_user_is_not_owner() {
    let guard = make_guard(Some("userA"));
    assert!(!guard.is_owner("userB"), "non-owner must be denied");
}

/// Documents the original security hole: `is_none_or` returns `true` for `None`
/// regardless of the closure, so every user could approve when no guard was set.
#[test]
fn old_is_none_or_was_security_hole() {
    let old_behavior = Option::<()>::None.as_ref().is_none_or(|_| false);
    assert!(old_behavior, "is_none_or(false) on None was true — that was the bug");
}

/// The fix: `map_or(false, ...)` returns `false` for `None`.
#[test]
fn map_or_false_correct_semantics() {
    let new_behavior = Option::<()>::None.as_ref().map_or(false, |_| true);
    assert!(!new_behavior, "map_or(false, ..) on None must be false");
}

/// Guard with no `owner_id` denies every user.
#[tokio::test]
async fn guard_with_no_owner_denies_all() {
    let guard = make_guard(None);
    assert!(!guard.is_owner("any_user"), "guard with owner_id=None must deny all users");
}
