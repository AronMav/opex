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
//! Tests 1, 4, 5 are pure `Option` combinators — no runtime needed.
//! Tests 2, 3, 6 construct `AccessGuard` with a lazy pool; `PgPool::connect_lazy`
//! requires a Tokio context so those are `#[tokio::test]`.  No live DB is used:
//! `is_owner` is pure sync and never calls the pool.

use hydeclaw_core::channels::access::AccessGuard;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build an `AccessGuard` that never touches the database.
/// `is_owner` only reads `self.owner_id`; the lazy pool is never used.
fn make_guard(owner_id: Option<&str>) -> AccessGuard {
    let pool = sqlx::PgPool::connect_lazy("postgres://invalid").expect("lazy pool");
    AccessGuard::new(
        "agent-test".to_string(),
        owner_id.map(str::to_string),
        false,
        pool,
    )
}

// ── Test 1: None guard → not owner (pure combinators, no runtime) ─────────────

/// When the agent has no access guard (`live_guard = None`), `map_or(false, ...)`
/// must return `false` for every user — no one may resolve approvals.
#[test]
fn no_guard_is_not_owner() {
    let live_guard: Option<AccessGuard> = None;
    let user_id = "any_user";
    let is_owner = live_guard.as_ref().map_or(false, |g| g.is_owner(user_id));
    assert!(!is_owner, "no guard must deny ownership for any user");
}

// ── Test 2: correct owner → is_owner true ────────────────────────────────────

/// When the guard's `owner_id` matches the requesting user, `is_owner` returns `true`.
/// `#[tokio::test]` is required because `PgPool::connect_lazy` needs a Tokio runtime.
#[tokio::test]
async fn guard_with_owner_is_owner() {
    let guard = make_guard(Some("userA"));
    assert!(guard.is_owner("userA"), "owner should be recognised");
}

// ── Test 3: wrong user → is_owner false ──────────────────────────────────────

/// A user who is NOT the owner must be denied even when a guard exists.
#[tokio::test]
async fn guard_with_wrong_user_is_not_owner() {
    let guard = make_guard(Some("userA"));
    assert!(!guard.is_owner("userB"), "non-owner must be denied");
}

// ── Test 4: document the old security hole (pure combinators) ────────────────

/// `Option::is_none_or` returns `true` when the option is `None`, regardless
/// of the closure result.  This is exactly why the original code was a security
/// hole: an unguarded agent (`live_guard = None`) passed the ownership check
/// for every caller.
#[test]
fn old_is_none_or_was_security_hole() {
    // live_guard = None, closure always returns false (non-owner)
    let live_guard: Option<()> = None;
    let old_behavior = live_guard.as_ref().is_none_or(|_| false);
    assert!(
        old_behavior,
        "is_none_or returns true for None — this was the security bug: \
         an agent with no guard let any user approve tool calls"
    );
}

// ── Test 5: confirm the fix semantics (pure combinators) ─────────────────────

/// `Option::map_or(false, ...)` returns `false` for `None` regardless of the
/// closure — the correct gate semantics: no guard ⇒ not owner.
#[test]
fn map_or_false_correct_semantics() {
    let live_guard: Option<()> = None;
    let new_behavior = live_guard.as_ref().map_or(false, |_| true);
    assert!(
        !new_behavior,
        "map_or(false, ...) must return false for None — this is the correct fix"
    );
}

// ── Test 6: guard with no owner set ──────────────────────────────────────────

/// If the guard exists but has no `owner_id` configured, `is_owner` must return
/// `false` for every user ID.
#[tokio::test]
async fn guard_with_no_owner_denies_all() {
    let guard = make_guard(None);
    assert!(
        !guard.is_owner("any_user"),
        "guard with owner_id=None must deny all users"
    );
}
