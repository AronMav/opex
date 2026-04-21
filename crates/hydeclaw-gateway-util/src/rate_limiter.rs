//! Per-IP rate limiters shared by the auth + request middleware.
//!
//! Phase 62 RES-04 extracted these types from `middleware.rs` into a leaf
//! module (zero `crate::*` imports) so the test-facing `lib.rs` facade can
//! re-export them without cascading the gateway handler subtree. The
//! public API (`AuthRateLimiter`, `RequestRateLimiter`) is preserved;
//! `middleware.rs` re-exports both via `pub use`.
//!
//! Eviction semantics: `sweep()` is called every 60s by a background tokio
//! task spawned in `gateway::router()`. The hot path (`record_failure`,
//! `check`) no longer scans the HashMap — Phase 62 RES-04.

use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::Mutex;

/// Tracks failed auth attempts per IP. After `max_attempts` failures within the window,
/// the IP is locked out for `lockout_secs` seconds.
pub struct AuthRateLimiter {
    #[allow(dead_code)]
    max_attempts: u32,
    lockout_secs: u64,
    /// IP → (`fail_count`, `first_fail_time`, `locked_until`)
    #[allow(clippy::type_complexity)]
    state: Mutex<HashMap<String, (u32, Instant, Option<Instant>)>>,
}

impl AuthRateLimiter {
    pub fn new(max_attempts: u32, lockout_secs: u64) -> Self {
        Self {
            max_attempts,
            lockout_secs,
            state: Mutex::new(HashMap::new()),
        }
    }

    #[allow(dead_code)]
    pub async fn is_locked(&self, ip: &str) -> bool {
        let state = self.state.lock().await;
        if let Some((_, _, Some(locked_until))) = state.get(ip)
            && Instant::now() < *locked_until {
                return true;
            }
        false
    }

    #[allow(dead_code)]
    pub async fn record_failure(&self, ip: &str) {
        let mut state = self.state.lock().await;
        let now = Instant::now();

        // Phase 62 RES-04: inline eviction removed — background sweeper handles it.

        let entry = state.entry(ip.to_string()).or_insert((0, now, None));

        // Reset if previous window expired (no lockout active)
        if entry.2.is_none() && now.duration_since(entry.1).as_secs() > self.lockout_secs {
            *entry = (0, now, None);
        }

        entry.0 += 1;
        if entry.0 >= self.max_attempts {
            let lockout_until = now + std::time::Duration::from_secs(self.lockout_secs);
            entry.2 = Some(lockout_until);
            tracing::warn!(ip = %ip, "auth rate limit: IP locked for {}s after {} failed attempts", self.lockout_secs, self.max_attempts);
        }
    }

    #[allow(dead_code)]
    pub async fn record_success(&self, ip: &str) {
        let mut state = self.state.lock().await;
        state.remove(ip);
    }

    /// Phase 62 RES-04: evict expired entries. Called by the background
    /// sweeper task (spawned in gateway::mod.rs) every 60 seconds.
    /// Replaces the per-write inline eviction that scaled with HashMap size.
    pub async fn sweep(&self) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        let lockout = std::time::Duration::from_secs(self.lockout_secs);
        state.retain(|_, (_, first_fail, locked_until)| {
            if let Some(until) = locked_until {
                now < *until // keep if still locked out
            } else {
                now.duration_since(*first_fail) < lockout // keep if window active
            }
        });
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
        let mut state = self.state.lock().await;
        state.insert(ip.to_string(), (1, first_fail, locked_until));
    }

    #[doc(hidden)]
    pub async fn __test_len(&self) -> usize {
        self.state.lock().await.len()
    }

    /// Phase 65 OBS-05: snapshot current map size for `/api/health/dashboard`.
    /// Takes a shared async lock briefly; safe to call from a hot-path
    /// request handler because the background sweeper keeps the map bounded.
    pub async fn snapshot_size(&self) -> usize {
        self.state.lock().await.len()
    }
}

/// Per-IP request rate limiter using a fixed-window counter.
/// Protects the Pi from overload by limiting requests per minute.
pub struct RequestRateLimiter {
    #[allow(dead_code)]
    pub max_per_minute: u32,
    /// IP → (`request_count`, `window_start`)
    state: Mutex<HashMap<String, (u32, Instant)>>,
}

impl RequestRateLimiter {
    pub fn new(max_per_minute: u32) -> Self {
        Self {
            max_per_minute,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Returns Ok(()) if allowed, `Err(seconds_until_reset)` if rate-limited.
    #[allow(dead_code)]
    pub async fn check(&self, ip: &str) -> std::result::Result<(), u64> {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);

        // Phase 62 RES-04: inline eviction removed — background sweeper handles it.

        let entry = state.entry(ip.to_string()).or_insert((0, now));

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
    pub async fn sweep(&self) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);
        state.retain(|_, (_, start)| now.duration_since(*start) < window);
    }

    #[doc(hidden)]
    pub async fn __test_insert(&self, ip: &str, start: Instant) {
        let mut state = self.state.lock().await;
        state.insert(ip.to_string(), (1, start));
    }

    #[doc(hidden)]
    pub async fn __test_len(&self) -> usize {
        self.state.lock().await.len()
    }

    /// Phase 65 OBS-05: snapshot current map size for `/api/health/dashboard`.
    /// Same bounded-lock semantics as [`AuthRateLimiter::snapshot_size`].
    pub async fn snapshot_size(&self) -> usize {
        self.state.lock().await.len()
    }
}
