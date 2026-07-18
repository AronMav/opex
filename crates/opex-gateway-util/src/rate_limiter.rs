#![deny(clippy::await_holding_lock)]
//! Per-IP rate limiters shared by the auth + request middleware.
//!
//! Phase 62 RES-04 extracted these types from `middleware.rs` into a leaf
//! module (zero `crate::*` imports) so the test-facing `lib.rs` facade can
//! re-export them without cascading the gateway handler subtree. The
//! public API (`AuthRateLimiter`, `RequestRateLimiter`) is preserved;
//! `middleware.rs` re-exports both via `pub use`.
//!
//! Phase 67 REF-03: swapped `Mutex<HashMap>` for `DashMap` (sharded sync
//! locks, no tokio runtime suspension on contention). Guard-across-await is
//! enforced at compile time by `#![deny(clippy::await_holding_lock)]`.
//! Sweeper uses collect-keys-then-remove (not `retain`) per REF-03 criterion 3.
//!
//! Eviction semantics: `sweep()` is called every 60s by a background tokio
//! task spawned in `gateway::router()`. The hot path (`record_failure`,
//! `check`) no longer scans the map — Phase 62 RES-04.

use dashmap::DashMap;
use std::time::{Duration, Instant};

/// Tracks failed auth attempts per IP. After `max_attempts` failures within the window,
/// the IP is locked out for `lockout_secs` seconds.
pub struct AuthRateLimiter {
    max_attempts: u32,
    lockout_secs: u64,
    /// IP → (`fail_count`, `first_fail_time`, `locked_until`)
    ///
    /// Phase 67 REF-03: DashMap replaces `Mutex<HashMap>`. Each shard is
    /// independently locked, eliminating tokio thread suspension on contention.
    #[allow(clippy::type_complexity)]
    state: DashMap<String, (u32, Instant, Option<Instant>)>,
}

impl AuthRateLimiter {
    pub fn new(max_attempts: u32, lockout_secs: u64) -> Self {
        Self {
            max_attempts,
            lockout_secs,
            state: DashMap::new(),
        }
    }

    pub async fn is_locked(&self, ip: &str) -> bool {
        // Fetch-clone-drop: the Ref guard is released at the end of this let.
        let locked_until = self.state.get(ip).and_then(|e| e.2);
        if let Some(until) = locked_until {
            return Instant::now() < until;
        }
        false
    }

    pub async fn record_failure(&self, ip: &str) {
        let now = Instant::now();
        let mut entry = self
            .state
            .entry(ip.to_string())
            .or_insert((0, now, None));

        // Reset if previous window expired (no lockout active)
        if entry.2.is_none() && now.duration_since(entry.1).as_secs() > self.lockout_secs {
            *entry = (0, now, None);
        }

        entry.0 += 1;
        if entry.0 >= self.max_attempts {
            let lockout_until = now + Duration::from_secs(self.lockout_secs);
            entry.2 = Some(lockout_until);
            tracing::warn!(
                ip = %ip,
                "auth rate limit: IP locked for {}s after {} failed attempts",
                self.lockout_secs,
                self.max_attempts
            );
        }
        // entry guard drops here automatically
    }

    pub async fn record_success(&self, ip: &str) {
        self.state.remove(ip);
    }

    /// Phase 62 RES-04: evict expired entries. Called by the background
    /// sweeper task (spawned in gateway::mod.rs) every 60 seconds.
    /// Replaces the per-write inline eviction that scaled with map size.
    ///
    /// Phase 67 REF-03: uses collect-keys-then-remove (not `retain`) so
    /// each eviction is an independent shard lock, not a full-map hold.
    pub async fn sweep(&self) {
        let now = Instant::now();
        let lockout = Duration::from_secs(self.lockout_secs);
        let to_remove: Vec<String> = self
            .state
            .iter()
            .filter_map(|r| {
                let (_, first_fail, locked_until) = *r.value();
                let expired = if let Some(until) = locked_until {
                    now >= until
                } else {
                    now.duration_since(first_fail) >= lockout
                };
                if expired {
                    Some(r.key().clone())
                } else {
                    None
                }
            })
            .collect();
        for key in to_remove {
            self.state.remove(&key);
        }
    }

    /// Test backdoor — insert a synthetic entry. NOT part of the public API;
    /// prefixed __ to signal this. Used by `integration_rate_limiter_sweeper.rs`.
    #[doc(hidden)]
    pub async fn __test_insert(
        &self,
        ip: &str,
        first_fail: Instant,
        locked_until: Option<Instant>,
    ) {
        self.state
            .insert(ip.to_string(), (1, first_fail, locked_until));
    }

    #[doc(hidden)]
    pub async fn __test_len(&self) -> usize {
        self.state.len()
    }

    /// Phase 65 OBS-05: snapshot current map size for `/api/health/dashboard`.
    /// Phase 67 REF-03: DashMap shard read; effectively O(shard_count) wall-clock
    /// — safe to call from a hot-path request handler because the background
    /// sweeper keeps the map bounded.
    pub async fn snapshot_size(&self) -> usize {
        self.state.len()
    }
}

/// Per-IP request rate limiter using a fixed-window counter.
/// Protects the Pi from overload by limiting requests per minute.
pub struct RequestRateLimiter {
    pub max_per_minute: u32,
    /// IP → (`request_count`, `window_start`)
    ///
    /// Phase 67 REF-03: DashMap replaces `Mutex<HashMap>`.
    state: DashMap<String, (u32, Instant)>,
}

impl RequestRateLimiter {
    pub fn new(max_per_minute: u32) -> Self {
        Self {
            max_per_minute,
            state: DashMap::new(),
        }
    }

    /// Returns Ok(()) if allowed, `Err(seconds_until_reset)` if rate-limited.
    pub async fn check(&self, ip: &str) -> std::result::Result<(), u64> {
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut entry = self.state.entry(ip.to_string()).or_insert((0, now));

        // Reset window if expired
        if now.duration_since(entry.1) >= window {
            *entry = (0, now);
        }

        entry.0 += 1;

        if entry.0 > self.max_per_minute {
            let elapsed = now.duration_since(entry.1).as_secs();
            let retry_after = 60u64.saturating_sub(elapsed);
            Err(retry_after)
        } else {
            Ok(())
        }
    }

    /// Phase 62 RES-04: evict stale entries past the 60s window.
    ///
    /// Phase 67 REF-03: uses collect-keys-then-remove (not `retain`).
    pub async fn sweep(&self) {
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let to_remove: Vec<String> = self
            .state
            .iter()
            .filter_map(|r| {
                if now.duration_since(r.value().1) >= window {
                    Some(r.key().clone())
                } else {
                    None
                }
            })
            .collect();
        for key in to_remove {
            self.state.remove(&key);
        }
    }

    #[doc(hidden)]
    pub async fn __test_insert(&self, ip: &str, start: Instant) {
        self.state.insert(ip.to_string(), (1, start));
    }

    #[doc(hidden)]
    pub async fn __test_len(&self) -> usize {
        self.state.len()
    }

    /// Phase 65 OBS-05: snapshot current map size for `/api/health/dashboard`.
    /// Phase 67 REF-03: DashMap shard read; same bounded semantics as
    /// [`AuthRateLimiter::snapshot_size`].
    pub async fn snapshot_size(&self) -> usize {
        self.state.len()
    }
}

/// True when the request carries `Authorization: Bearer <token>` exactly
/// matching the gateway auth token (constant-time compare).
///
/// Used by the request rate limiter middleware to exempt authenticated
/// callers: the per-IP budget shields the server from anonymous abuse, while
/// the web UI's own polling (tasks/sessions/notifications across several
/// open tabs) legitimately exceeds a small budget — throttling it surfaces
/// as 429 storms in the browser. Invalid or absent tokens still consume the
/// budget, so the exemption cannot be triggered by garbage headers.
pub fn valid_bearer(headers: &axum::http::HeaderMap, expected_token: &str) -> bool {
    use subtle::ConstantTimeEq;
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|t| bool::from(t.as_bytes().ct_eq(expected_token.as_bytes())))
}
