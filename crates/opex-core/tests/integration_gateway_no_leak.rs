//! Phase 66 REF-06 regression guard.
//!
//! ROADMAP Phase 66 success criterion #6 is satisfied by either "allocator
//! delta" OR "absence of Box::leak in the source" — this test picks the
//! latter (faster, zero-flake, CI-friendly) and adds a 100× Arc::new+drop
//! loop as a supplementary sanity check on the Arc lifecycle.
//!
//! Guards against:
//!   * Re-introduction of `Box::leak` in `gateway/mod.rs`.
//!   * Accidental swap from stdlib `std::sync::OnceLock` to `once_cell`
//!     (CONTEXT.md: stdlib-only since Rust 1.70 covers the need).
//!   * Silent Arc leak on the happy path — 100 construct/drop cycles and
//!     assert the Weak handle cannot upgrade after `drop(arc)`.

use std::sync::Arc;

/// Primary Phase 66 REF-06 guarantee: gateway/mod.rs must not contain
/// `Box::leak` anywhere — including inside comments. Plan accepted this
/// stricter reading over a regex that ignores comments because the
/// easiest way to make the source "contain" the literal again is to
/// paste an old snippet back into a comment during review.
#[test]
fn gateway_mod_has_no_box_leak() {
    let src = include_str!("../src/gateway/mod.rs");
    assert!(
        !src.contains("Box::leak"),
        "REF-06: Box::leak must not reappear in gateway/mod.rs"
    );
}

/// Secondary guard: the replacement pattern MUST be
/// `std::sync::OnceLock<Arc<T>>`. Also asserts that `once_cell` is not
/// imported — CONTEXT.md is explicit that REF-06 is stdlib-only.
#[test]
fn gateway_mod_uses_oncelock_arc_stdlib_only() {
    let src = include_str!("../src/gateway/mod.rs");
    assert!(
        src.contains("OnceLock<Arc<"),
        "REF-06: gateway/mod.rs must hold rate limiters as OnceLock<Arc<T>> (stdlib)"
    );
    assert!(
        !src.contains("use once_cell"),
        "REF-06 is stdlib-only per CONTEXT.md — no once_cell dep allowed here"
    );
}

/// Sanity check on the Arc lifecycle: 100 instantiate/drop cycles against
/// the real `AuthRateLimiter` / `RequestRateLimiter` constructors (via the
/// lib facade). Each cycle snapshots a `Weak` before `drop(arc)` and
/// asserts `upgrade()` returns `None` — i.e. the Arc's strong-count went
/// to zero exactly when the last owner dropped.
///
/// This does NOT exercise the gateway's module-level `OnceLock` statics
/// (those are program-lifetime by design — the whole point of moving off
/// `Box::leak` is that the Arc inside the OnceLock behaves normally, not
/// that the OnceLock slot itself is reclaimed). The test guards the
/// primary failure mode Phase 66 REF-06 actually replaces: an Arc that
/// fails to drop when its last owner goes away.
#[test]
fn arc_rate_limiters_lifecycle_no_leak_across_100_cycles() {
    use opex_core::gateway::rate_limiter::{AuthRateLimiter, RequestRateLimiter};
    for i in 0..100 {
        let auth = Arc::new(AuthRateLimiter::new(500, 30));
        let req = Arc::new(RequestRateLimiter::new(300));
        let weak_auth = Arc::downgrade(&auth);
        let weak_req = Arc::downgrade(&req);
        drop(auth);
        drop(req);
        assert!(
            weak_auth.upgrade().is_none(),
            "Arc<AuthRateLimiter> leaked after drop (cycle {i})"
        );
        assert!(
            weak_req.upgrade().is_none(),
            "Arc<RequestRateLimiter> leaked after drop (cycle {i})"
        );
    }
}
