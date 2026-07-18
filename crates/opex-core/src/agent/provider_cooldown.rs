//! Process-wide provider cooldown registry (Session Resilience Task 4 / WS4).
//!
//! G2 scope: consulted only BETWEEN LLM calls / at turn start — never
//! mid-stream. In-memory by design (single-binary gateway); no persistence,
//! no background sweep, no timer. Expiry is "self-healing": `is_cooled`
//! simply returns `false` once `Instant::now()` has passed the recorded
//! `cooldown_until`, evicting the stale entry lazily on that same read.
//!
//! Shared as ONE process-wide instance: constructed once in `main.rs`,
//! carried on `AgentDeps.cooldowns` (mirrors the `tool_exec_ctx` /
//! `checkpoint_mgr` precedent), and cloned onto every `AgentConfig.cooldowns`
//! at agent-start time (`gateway/handlers/agents/lifecycle.rs`) — so every
//! agent that happens to share a provider name observes the same cooldown
//! state, not a per-agent copy.

use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::agent::error_classify::{self, LlmErrorClass};
use crate::db::profiles::SlotEntry;

/// Tracks a `cooldown_until` `Instant` per provider name.
pub struct ProviderCooldowns {
    map: DashMap<String, Instant>,
}

impl Default for ProviderCooldowns {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderCooldowns {
    pub fn new() -> Self {
        Self { map: DashMap::new() }
    }

    /// Record a failover-worthy failure: `cooldown_until = now +
    /// cooldown_duration(class)`. Callers gate this on
    /// `error_classify::is_failover_worthy(class)` at the call site
    /// (`pipeline::execute`) — recording a zero-cooldown class
    /// (`ContextOverflow` / `SessionCorruption` / `CallTimeout`) here is
    /// harmless either way since `is_cooled` immediately reports `false`
    /// for a zero-length window.
    pub fn record_failure(&self, provider_name: &str, class: &LlmErrorClass) {
        let dur = error_classify::cooldown_duration(class);
        self.insert_cooldown(provider_name, dur);
    }

    /// Clear on success (primary healed).
    pub fn record_success(&self, provider_name: &str) {
        self.map.remove(provider_name);
    }

    /// True while `now < cooldown_until`. Expired entries are evicted lazily
    /// on the read that discovers them stale — no background sweep, no
    /// timer. This lazy eviction IS the primary's self-heal: each new turn's
    /// `is_cooled` check naturally flips back to `false` once the window
    /// passes, with no extra bookkeeping.
    pub fn is_cooled(&self, provider_name: &str) -> bool {
        let Some(entry) = self.map.get(provider_name) else {
            return false;
        };
        let until = *entry;
        drop(entry); // release the shard read-guard before the possible remove() below
        if Instant::now() < until {
            true
        } else {
            self.map.remove(provider_name);
            false
        }
    }

    fn insert_cooldown(&self, provider_name: &str, dur: Duration) {
        self.map.insert(provider_name.to_string(), Instant::now() + dur);
    }
}

#[cfg(test)]
impl ProviderCooldowns {
    /// Test-only: set an explicit cooldown duration, bypassing the
    /// class→duration lookup table entirely — lets tests use
    /// millisecond-scale windows instead of sleeping through the real 60s
    /// `RateLimit` cooldown.
    pub(crate) fn record_failure_for(&self, provider_name: &str, dur: Duration) {
        self.insert_cooldown(provider_name, dur);
    }
}

/// Resolve the next usable (non-cooled) entry in a profile's `text` reserve
/// chain, starting the search at `chain_idx` — i.e. examining `chain[1 +
/// chain_idx]`, `chain[2 + chain_idx]`, … (mirrors the engine's `text[1 +
/// chain_idx]` indexing convention, `chain[0]` being the live primary and
/// never a candidate here). Returns the entry plus the chain position
/// actually used, so the caller can seed `fallback_chain_idx` at that
/// position before its own `+1` bump
/// (`behaviour::LayerRuntimeState::adopt_fallback`).
///
/// `None` when every remaining position — cooled or not — is unusable: the
/// chain is exhausted, or every reserve from `chain_idx` onward is cooled.
pub fn resolve_next_uncooled<'a>(
    chain: &'a [SlotEntry],
    cooldowns: &ProviderCooldowns,
    chain_idx: usize,
) -> Option<(&'a SlotEntry, usize)> {
    let mut idx = chain_idx;
    loop {
        let entry = chain.get(1 + idx)?;
        if !cooldowns.is_cooled(&entry.provider) {
            return Some((entry, idx));
        }
        idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_provider_is_not_cooled() {
        let c = ProviderCooldowns::new();
        assert!(!c.is_cooled("openai"));
    }

    #[test]
    fn record_failure_rate_limit_cools_the_provider() {
        let c = ProviderCooldowns::new();
        c.record_failure("openai", &LlmErrorClass::RateLimit);
        assert!(c.is_cooled("openai"));
    }

    #[test]
    fn record_success_clears_cooldown() {
        let c = ProviderCooldowns::new();
        c.record_failure("openai", &LlmErrorClass::RateLimit);
        assert!(c.is_cooled("openai"));
        c.record_success("openai");
        assert!(!c.is_cooled("openai"));
    }

    #[test]
    fn record_success_on_a_never_cooled_provider_is_a_harmless_noop() {
        let c = ProviderCooldowns::new();
        c.record_success("openai");
        assert!(!c.is_cooled("openai"));
    }

    #[test]
    fn cooldown_expires_after_its_duration_then_stays_expired() {
        let c = ProviderCooldowns::new();
        c.record_failure_for("openai", Duration::from_millis(20));
        assert!(c.is_cooled("openai"), "should be cooled immediately after recording");
        std::thread::sleep(Duration::from_millis(80));
        assert!(!c.is_cooled("openai"), "cooldown should have expired and been evicted lazily");
        // Second read confirms the lazy-evicted entry doesn't resurrect itself.
        assert!(!c.is_cooled("openai"));
    }

    #[test]
    fn zero_duration_class_never_actually_cools_the_provider() {
        let c = ProviderCooldowns::new();
        // ContextOverflow / SessionCorruption / CallTimeout map to
        // Duration::ZERO in `error_classify::cooldown_duration` — recording
        // one must not leave the provider observably cooled.
        c.record_failure("openai", &LlmErrorClass::ContextOverflow);
        assert!(!c.is_cooled("openai"));
        c.record_failure("openai", &LlmErrorClass::SessionCorruption);
        assert!(!c.is_cooled("openai"));
        c.record_failure("openai", &LlmErrorClass::CallTimeout);
        assert!(!c.is_cooled("openai"));
    }

    #[test]
    fn different_providers_have_independent_cooldowns() {
        let c = ProviderCooldowns::new();
        c.record_failure("openai", &LlmErrorClass::RateLimit);
        assert!(c.is_cooled("openai"));
        assert!(!c.is_cooled("anthropic"), "a different provider name must be unaffected");
    }

    #[test]
    fn a_later_failure_overwrites_an_earlier_shorter_cooldown() {
        let c = ProviderCooldowns::new();
        c.record_failure_for("openai", Duration::from_millis(20));
        c.record_failure("openai", &LlmErrorClass::RateLimit); // 60s — overwrites
        std::thread::sleep(Duration::from_millis(40));
        assert!(c.is_cooled("openai"), "the longer, more recent cooldown should still be in effect");
    }

    // ── resolve_next_uncooled ──

    fn chain() -> Vec<SlotEntry> {
        vec![
            SlotEntry { provider: "primary".into(), model: None, voice: None },
            SlotEntry { provider: "reserve1".into(), model: None, voice: None },
            SlotEntry { provider: "reserve2".into(), model: None, voice: None },
        ]
    }

    #[test]
    fn resolve_next_uncooled_returns_first_reserve_when_nothing_is_cooled() {
        let c = ProviderCooldowns::new();
        let chain = chain();
        let (entry, idx) = resolve_next_uncooled(&chain, &c, 0).expect("reserve #1 present");
        assert_eq!(entry.provider, "reserve1");
        assert_eq!(idx, 0);
    }

    #[test]
    fn resolve_next_uncooled_skips_a_cooled_reserve() {
        let c = ProviderCooldowns::new();
        c.record_failure("reserve1", &LlmErrorClass::RateLimit);
        let chain = chain();
        let (entry, idx) = resolve_next_uncooled(&chain, &c, 0).expect("reserve #2 present");
        assert_eq!(entry.provider, "reserve2", "cooled reserve1 must be skipped");
        assert_eq!(idx, 1, "resolved index reflects the position actually used");
    }

    #[test]
    fn resolve_next_uncooled_returns_none_when_all_remaining_are_cooled() {
        let c = ProviderCooldowns::new();
        c.record_failure("reserve1", &LlmErrorClass::RateLimit);
        c.record_failure("reserve2", &LlmErrorClass::RateLimit);
        let chain = chain();
        assert!(resolve_next_uncooled(&chain, &c, 0).is_none());
    }

    #[test]
    fn resolve_next_uncooled_returns_none_when_chain_exhausted() {
        let c = ProviderCooldowns::new();
        let chain = chain();
        // chain_idx=2 → chain.get(1+2)=chain.get(3) → out of bounds.
        assert!(resolve_next_uncooled(&chain, &c, 2).is_none());
    }
}
