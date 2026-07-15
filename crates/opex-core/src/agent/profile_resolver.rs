//! Резолюция профиля агента в цепочки провайдеров по capability.
//! Профиль не найден → Default + warn; нет и Default → пустые слоты
//! (агент без text-слота получает UnconfiguredProvider — существующий sentinel).

use crate::db::profiles::{Slots, SlotEntry, DEFAULT_PROFILE};
use sqlx::PgPool;
use std::collections::HashMap;

pub async fn resolve_slots_for_agent(db: &PgPool, profile_name: &str, agent_name: &str) -> Slots {
    match crate::db::profiles::get_profile_by_name(db, profile_name).await {
        Ok(Some(p)) => return p.parsed_slots(),
        Ok(None) => tracing::warn!(agent = %agent_name, profile = %profile_name,
            "agent profile not found; falling back to Default"),
        Err(e) => tracing::error!(agent = %agent_name, profile = %profile_name, error = %e,
            "profile lookup failed; falling back to Default"),
    }
    match crate::db::profiles::get_profile_by_name(db, DEFAULT_PROFILE).await {
        Ok(Some(p)) => p.parsed_slots(),
        _ => {
            tracing::error!(agent = %agent_name, "Default profile missing; all capabilities disabled");
            Slots::new()
        }
    }
}

/// Fetch every profile's slots in a single round trip, keyed by profile name.
/// Used by list endpoints (e.g. `GET /api/agents`) that need to resolve
/// capabilities for N agents without paying an N+1 query cost — pair with
/// `resolve_slots_offline` to look up each agent's profile in the returned map.
pub async fn load_all_profile_slots(db: &PgPool) -> HashMap<String, Slots> {
    match crate::db::profiles::list_profiles(db).await {
        Ok(rows) => rows.into_iter().map(|p| (p.name.clone(), p.parsed_slots())).collect(),
        Err(e) => {
            tracing::error!(error = %e, "list_profiles failed; capabilities will be empty for this request");
            HashMap::new()
        }
    }
}

/// Same fallback rule as `resolve_slots_for_agent` (profile → Default →
/// empty) but resolved against an already-fetched map instead of hitting the
/// DB — the synchronous counterpart used once `load_all_profile_slots` has
/// populated the map.
pub fn resolve_slots_offline(profiles: &HashMap<String, Slots>, profile_name: &str) -> Slots {
    profiles
        .get(profile_name)
        .or_else(|| profiles.get(DEFAULT_PROFILE))
        .cloned()
        .unwrap_or_default()
}

/// Цепочка слота, отфильтрованная по `providers.enabled`. Пустой результат =
/// возможность выключена.
pub async fn effective_chain(db: &PgPool, slots: &Slots, capability: &str) -> Vec<SlotEntry> {
    let Some(entries) = slots.get(capability) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries {
        match crate::db::providers::get_provider_by_name(db, &e.provider).await {
            Ok(Some(row)) if row.enabled => out.push(e.clone()),
            Ok(Some(_)) => tracing::debug!(provider = %e.provider, capability, "slot entry disabled, skipping"),
            Ok(None) => tracing::warn!(provider = %e.provider, capability, "slot entry provider missing, skipping"),
            Err(err) => tracing::warn!(provider = %e.provider, capability, error = %err, "slot entry lookup failed, skipping"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_slots_offline (sync, no DB needed) ──────────────────────────

    #[test]
    fn resolve_slots_offline_uses_matching_profile() {
        let mut profiles = HashMap::new();
        let mut slots = Slots::new();
        slots.insert("stt".into(), vec![SlotEntry { provider: "w".into(), model: None, voice: None }]);
        profiles.insert("Custom".to_string(), slots);
        let got = resolve_slots_offline(&profiles, "Custom");
        assert!(got.contains_key("stt"));
    }

    #[test]
    fn resolve_slots_offline_falls_back_to_default() {
        let mut profiles = HashMap::new();
        let mut slots = Slots::new();
        slots.insert("tts".into(), vec![SlotEntry { provider: "v".into(), model: None, voice: None }]);
        profiles.insert(DEFAULT_PROFILE.to_string(), slots);
        let got = resolve_slots_offline(&profiles, "Ghost");
        assert!(got.contains_key("tts"));
    }

    #[test]
    fn resolve_slots_offline_empty_when_nothing_matches() {
        let profiles: HashMap<String, Slots> = HashMap::new();
        let got = resolve_slots_offline(&profiles, "Ghost");
        assert!(got.is_empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn missing_profile_falls_back_to_default(pool: sqlx::PgPool) {
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert("stt".into(), vec![crate::db::profiles::SlotEntry {
            provider: "w".into(), model: None, voice: None }]);
        crate::db::profiles::create_profile(&pool, "Default", &slots).await.unwrap();
        let got = resolve_slots_for_agent(&pool, "Ghost", "Arty").await;
        assert!(got.contains_key("stt"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn no_profiles_at_all_gives_empty(pool: sqlx::PgPool) {
        let got = resolve_slots_for_agent(&pool, "Ghost", "Arty").await;
        assert!(got.is_empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn load_all_profile_slots_returns_every_profile_by_name(pool: sqlx::PgPool) {
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert("stt".into(), vec![crate::db::profiles::SlotEntry {
            provider: "w".into(), model: None, voice: None }]);
        crate::db::profiles::create_profile(&pool, "Default", &slots).await.unwrap();
        crate::db::profiles::create_profile(&pool, "Custom", &Slots::new()).await.unwrap();
        let map = load_all_profile_slots(&pool).await;
        assert!(map.contains_key("Default"));
        assert!(map.contains_key("Custom"));
        assert!(map["Default"].contains_key("stt"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn effective_chain_skips_disabled(pool: sqlx::PgPool) {
        sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES \
            ('a','tts','minimax',false),('b','tts','minimax',true)")
            .execute(&pool).await.unwrap();
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert("tts".into(), vec![
            crate::db::profiles::SlotEntry { provider: "a".into(), model: None, voice: None },
            crate::db::profiles::SlotEntry { provider: "b".into(), model: None, voice: None },
        ]);
        let chain = effective_chain(&pool, &slots, "tts").await;
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].provider, "b");
    }

    /// Mirrors the enabled-filtering loop in
    /// `gateway/handlers/agents/lifecycle.rs` (spec §8): a DISABLED leading
    /// `text` entry must be dropped so the stored slots have primary == raw
    /// text[1] and the first failover reserve == raw text[2]. This keeps
    /// `create_fallback_provider(chain_idx)`'s `text[1 + chain_idx]` indexing
    /// aligned with the live primary at `text[0]`.
    #[sqlx::test(migrations = "../../migrations")]
    async fn filtered_slots_drop_disabled_leader_and_align_reserves(pool: sqlx::PgPool) {
        // p_a disabled leader; p_b (primary) and p_c (reserve) enabled.
        sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES \
            ('p_a','llm','openai_compat',false),\
            ('p_b','llm','openai_compat',true),\
            ('p_c','llm','openai_compat',true)")
            .execute(&pool).await.unwrap();
        let mut raw_slots = crate::db::profiles::Slots::new();
        raw_slots.insert("text".into(), vec![
            crate::db::profiles::SlotEntry { provider: "p_a".into(), model: None, voice: None },
            crate::db::profiles::SlotEntry { provider: "p_b".into(), model: None, voice: None },
            crate::db::profiles::SlotEntry { provider: "p_c".into(), model: None, voice: None },
        ]);

        // Same shape as the engine-build filtering loop.
        let mut filtered = crate::db::profiles::Slots::new();
        for cap in crate::db::profiles::PROFILE_CAPABILITIES {
            let chain = effective_chain(&pool, &raw_slots, cap).await;
            if !chain.is_empty() {
                filtered.insert(cap.to_string(), chain);
            }
        }

        let text = &filtered["text"];
        assert_eq!(text.len(), 2, "disabled leader must be dropped");
        // Primary is text[0]; first failover (chain_idx 0) is text[1].
        assert_eq!(text[0].provider, "p_b", "primary == first enabled entry");
        assert_eq!(text[1].provider, "p_c", "first reserve == next enabled entry");
    }

    /// Common-path invariant: when every referenced provider is enabled, the
    /// filtered map equals the raw slots exactly (no behavioral change).
    #[sqlx::test(migrations = "../../migrations")]
    async fn filtered_slots_equal_raw_when_all_enabled(pool: sqlx::PgPool) {
        sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES \
            ('e_a','llm','openai_compat',true),('e_b','llm','openai_compat',true)")
            .execute(&pool).await.unwrap();
        let mut raw_slots = crate::db::profiles::Slots::new();
        raw_slots.insert("text".into(), vec![
            crate::db::profiles::SlotEntry { provider: "e_a".into(), model: None, voice: None },
            crate::db::profiles::SlotEntry { provider: "e_b".into(), model: None, voice: None },
        ]);

        let mut filtered = crate::db::profiles::Slots::new();
        for cap in crate::db::profiles::PROFILE_CAPABILITIES {
            let chain = effective_chain(&pool, &raw_slots, cap).await;
            if !chain.is_empty() {
                filtered.insert(cap.to_string(), chain);
            }
        }
        assert_eq!(filtered, raw_slots, "all-enabled: filtered == raw");
    }
}
