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

/// Called after a successful approve. Audit 2026-05-08 (4th pass): the
/// previous version simply removed the entry, which meant `(9 fails + 1
/// success) × N` was unbounded — the counter started over after every
/// successful guess. We now keep the counter and only clear the
/// active lockout, so the rolling 5-minute window still caps the number
/// of failed attempts regardless of intermixed successes. Once the
/// window slides past `first_failure` the counter naturally resets via
/// `record_pairing_failure`.
fn clear_pairing_failures(agent_id: &str) {
    if let Some(mut entry) = PAIRING_ATTEMPTS.get_mut(agent_id) {
        entry.locked_until = None;
    }
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
