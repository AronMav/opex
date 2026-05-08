use dashmap::DashMap;
use rand::Rng;
use sqlx::PgPool;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use crate::db::access;

// ── Pairing brute-force protection ───────────────────────────────────────────
//
// Audit 2026-05-08: a pairing code is 6 digits (10^6 possibilities) and a
// compromised loopback / channel adapter could enumerate it. Without a
// counter, a malicious adapter doing 1000 req/s would exhaust the keyspace
// in ~17 minutes — well inside the 5-minute pairing TTL when the attacker
// has already created a code in another window.
//
// We track failed attempts per agent_id (the pairing namespace). After
// `MAX_PAIRING_FAILURES` failures inside `PAIRING_FAILURE_WINDOW`, the
// agent's pairing endpoint locks out for `PAIRING_LOCKOUT_DURATION`. A
// successful approve clears the entry.

const MAX_PAIRING_FAILURES: u32 = 10;
const PAIRING_FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
const PAIRING_LOCKOUT_DURATION: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy)]
struct PairingAttempt {
    fail_count: u32,
    first_failure: Instant,
    locked_until: Option<Instant>,
}

static PAIRING_ATTEMPTS: LazyLock<DashMap<String, PairingAttempt>> =
    LazyLock::new(DashMap::new);

/// Returns `Some(seconds_remaining)` if the agent's pairing is currently
/// locked out due to recent failed attempts; `None` otherwise.
fn pairing_lockout_remaining(agent_id: &str) -> Option<u64> {
    let entry = PAIRING_ATTEMPTS.get(agent_id)?;
    let locked_until = entry.locked_until?;
    let now = Instant::now();
    if now < locked_until {
        Some(locked_until.duration_since(now).as_secs())
    } else {
        None
    }
}

fn record_pairing_failure(agent_id: &str) {
    let now = Instant::now();
    // Opportunistic GC: drop entries that have aged past the window and have
    // no active lockout. Cheap (DashMap shard scan) and bounded — only runs
    // on the (rare) failure path.
    sweep_stale_pairing_entries(now);
    let mut entry = PAIRING_ATTEMPTS
        .entry(agent_id.to_string())
        .or_insert(PairingAttempt {
            fail_count: 0,
            first_failure: now,
            locked_until: None,
        });
    if now.duration_since(entry.first_failure) > PAIRING_FAILURE_WINDOW {
        entry.fail_count = 0;
        entry.first_failure = now;
        entry.locked_until = None;
    }
    entry.fail_count += 1;
    if entry.fail_count >= MAX_PAIRING_FAILURES {
        entry.locked_until = Some(now + PAIRING_LOCKOUT_DURATION);
        tracing::warn!(
            agent_id = %agent_id,
            fail_count = entry.fail_count,
            "pairing locked out after repeated failures",
        );
    }
}

/// Called after a successful approve.
///
/// Audit 2026-05-08:
/// - 4th pass found that `remove(entry)` made the (9 fails + 1 success) × N
///   attack unbounded.
/// - 5th pass found that keeping `fail_count` after success creates a
///   different DoS: an attacker who races 9 wrong guesses then waits for a
///   legitimate user's success leaves `fail_count = 9`, and the legitimate
///   user's next typo trips the 10-fail lockout. The legitimate operator is
///   denied access by accumulated noise.
///
/// Resolution: a successful approve resets `fail_count = 0` AND sets
/// `first_failure = now`, but leaves the entry in the map. The 5-minute
/// rolling window therefore restarts cleanly from the success point, while
/// still preventing the 9+1 cycle attack — any subsequent burst of failures
/// has to climb back to 10 before lockout, but it no longer inherits the
/// pre-success counter.
fn clear_pairing_failures(agent_id: &str) {
    if let Some(mut entry) = PAIRING_ATTEMPTS.get_mut(agent_id) {
        entry.fail_count = 0;
        entry.first_failure = Instant::now();
        entry.locked_until = None;
    }
}

/// Best-effort sweep of stale `PAIRING_ATTEMPTS` entries: anything whose
/// rolling window expired AND has no active lockout is dropped. Called
/// opportunistically from `record_pairing_failure` so the map cannot grow
/// without bound on a long-running instance.
fn sweep_stale_pairing_entries(now: Instant) {
    PAIRING_ATTEMPTS.retain(|_, entry| {
        // Keep entries that are currently locked OR are still inside the
        // failure-counting window. Drop anything that has aged past the
        // window with no active lockout — those are inert state.
        entry.locked_until.is_some_and(|t| now < t)
            || now.duration_since(entry.first_failure) <= PAIRING_FAILURE_WINDOW
    });
}

/// Manages access control for a channel bot.
/// Pairing codes are stored in `PostgreSQL` (survive restarts).
pub struct AccessGuard {
    pub agent_id: String,
    pub(crate) owner_id: Option<String>,
    pub restricted: bool,
    pub(crate) db: PgPool,
}

impl AccessGuard {
    pub fn new(
        agent_id: String,
        owner_id: Option<String>,
        restricted: bool,
        db: PgPool,
    ) -> Self {
        Self { agent_id, owner_id, restricted, db }
    }

    /// Check if a user is allowed to use this bot.
    pub async fn is_allowed(&self, channel_user_id: &str) -> bool {
        if !self.restricted {
            return true;
        }
        if self.is_owner(channel_user_id) {
            return true;
        }
        access::is_user_allowed(&self.db, &self.agent_id, channel_user_id)
            .await
            .unwrap_or(false)
    }

    /// Check if a user is the owner.
    pub fn is_owner(&self, channel_user_id: &str) -> bool {
        self.owner_id.as_deref() == Some(channel_user_id)
    }

    /// Generate a 6-digit pairing code for an unknown user (persisted in DB).
    pub async fn create_pairing_code(
        &self,
        channel_user_id: &str,
        display_name: Option<&str>,
    ) -> String {
        let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
        if let Err(e) = access::store_pairing_code(
            &self.db, &self.agent_id, &code, channel_user_id, display_name,
        ).await {
            tracing::error!(error = %e, "failed to store pairing code in DB");
        }
        code
    }

    /// Try to approve a pairing by code.
    /// Returns (success, `user_display_info`).
    ///
    /// Rate-limited: after `MAX_PAIRING_FAILURES` failures in a rolling
    /// window, the agent's pairing locks out for `PAIRING_LOCKOUT_DURATION`.
    /// Successful approve resets the counter.
    pub async fn approve_pairing(&self, code: &str, approver_id: &str) -> (bool, String) {
        if let Some(remaining_secs) = pairing_lockout_remaining(&self.agent_id) {
            tracing::warn!(
                agent_id = %self.agent_id,
                remaining_secs,
                "pairing approve rejected: agent locked out",
            );
            return (false, format!("locked_out:{remaining_secs}"));
        }

        let result = match access::take_pairing_code(&self.db, &self.agent_id, code).await {
            Ok(Some((user_id, name, false))) => {
                let display = name.clone().unwrap_or_else(|| user_id.clone());
                if let Err(e) = access::add_allowed_user(
                    &self.db, &self.agent_id, &user_id, name.as_deref(), approver_id,
                ).await {
                    tracing::error!(error = %e, "failed to add allowed user");
                    (false, display)
                } else {
                    (true, display)
                }
            }
            Ok(Some((_, _, true))) => (false, "expired".to_string()),
            Ok(None) => (false, "not_found".to_string()),
            Err(e) => {
                tracing::error!(error = %e, "failed to take pairing code from DB");
                (false, "db_error".to_string())
            }
        };

        if result.0 {
            clear_pairing_failures(&self.agent_id);
        } else {
            record_pairing_failure(&self.agent_id);
        }
        result
    }

    /// Reject a pending pairing by code.
    pub async fn reject_pairing(&self, code: &str) -> bool {
        access::remove_pairing_code(&self.db, &self.agent_id, code)
            .await
            .unwrap_or(false)
    }

    /// List all pending pairing codes with user info (for UI display).
    pub async fn pending_pairings_list(&self) -> Vec<serde_json::Value> {
        match access::list_pairing_codes(&self.db, &self.agent_id).await {
            Ok(codes) => codes.iter().map(|p| {
                serde_json::json!({
                    "code": p.code,
                    "channel_user_id": p.channel_user_id,
                    "display_name": p.display_name,
                    "created_at": p.created_at.to_rfc3339(),
                })
            }).collect(),
            Err(e) => {
                tracing::error!(error = %e, "failed to list pairing codes");
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the pairing rate-limit helpers. These do NOT touch the
    //! database — they call the module-private fns directly. They run under a
    //! shared global `PAIRING_ATTEMPTS`, so every test uses a unique agent_id
    //! to avoid cross-test contamination.
    use super::*;

    fn unique_agent(name: &str) -> String {
        // Combine a per-test prefix with a high-resolution timestamp so two
        // tests using the same prefix still get distinct keys.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("test-pairing-{name}-{nanos}")
    }

    #[test]
    fn ten_failures_in_window_locks_out() {
        let aid = unique_agent("ten-fails");
        for _ in 0..10 {
            record_pairing_failure(&aid);
        }
        assert!(
            pairing_lockout_remaining(&aid).is_some(),
            "10 failures inside the rolling window must lock out",
        );
    }

    #[test]
    fn nine_then_success_then_fail_does_not_immediately_lock() {
        // Audit 2026-05-08 (5th pass) regression guard: previously
        // clear_pairing_failures kept fail_count at 9, so a single
        // post-success failure tripped lockout. Now success fully resets the
        // counter — the legitimate user gets the full failure budget back.
        let aid = unique_agent("nine-success-fail");
        for _ in 0..9 {
            record_pairing_failure(&aid);
        }
        assert!(pairing_lockout_remaining(&aid).is_none(), "9 failures alone must not lock");
        clear_pairing_failures(&aid);
        record_pairing_failure(&aid);
        assert!(
            pairing_lockout_remaining(&aid).is_none(),
            "post-success single failure must NOT lock (counter must have been reset)",
        );
    }

    #[test]
    fn cycle_attack_nine_plus_one_eventually_locks() {
        // The 4th-pass concern: 9 fail + 1 success ad infinitum. After the
        // 5th-pass fix, success resets the counter — so the attacker still
        // has to climb back from 0 each cycle. This test confirms the cycle
        // is bounded: 9 fails + clear, then 10 more fails inside the window
        // must lock.
        let aid = unique_agent("cycle");
        for _ in 0..9 {
            record_pairing_failure(&aid);
        }
        clear_pairing_failures(&aid);
        // Now climb from zero: 10 failures must lock.
        for _ in 0..10 {
            record_pairing_failure(&aid);
        }
        assert!(
            pairing_lockout_remaining(&aid).is_some(),
            "10 failures after a clear must still lock out",
        );
    }

    #[test]
    fn clear_pairing_failures_on_unknown_agent_is_noop() {
        // Must not panic / must not insert a phantom entry.
        let aid = unique_agent("nonexistent");
        assert!(PAIRING_ATTEMPTS.get(&aid).is_none());
        clear_pairing_failures(&aid);
        assert!(PAIRING_ATTEMPTS.get(&aid).is_none());
    }

    #[test]
    fn sweep_drops_stale_entries() {
        // Insert an entry with `first_failure` artificially far in the past
        // and no lockout, then call sweep — it must be dropped.
        let aid = unique_agent("stale");
        let long_ago = Instant::now() - (PAIRING_FAILURE_WINDOW + Duration::from_secs(60));
        PAIRING_ATTEMPTS.insert(
            aid.clone(),
            PairingAttempt {
                fail_count: 3,
                first_failure: long_ago,
                locked_until: None,
            },
        );
        sweep_stale_pairing_entries(Instant::now());
        assert!(
            PAIRING_ATTEMPTS.get(&aid).is_none(),
            "stale entry beyond window with no lockout should be swept",
        );
    }

    #[test]
    fn sweep_keeps_locked_entries() {
        let aid = unique_agent("locked");
        PAIRING_ATTEMPTS.insert(
            aid.clone(),
            PairingAttempt {
                fail_count: 10,
                first_failure: Instant::now() - Duration::from_secs(60),
                locked_until: Some(Instant::now() + Duration::from_secs(60)),
            },
        );
        sweep_stale_pairing_entries(Instant::now());
        assert!(
            PAIRING_ATTEMPTS.get(&aid).is_some(),
            "actively-locked entry must survive sweep regardless of window age",
        );
        // Cleanup so subsequent tests don't see this entry.
        PAIRING_ATTEMPTS.remove(&aid);
    }
}
