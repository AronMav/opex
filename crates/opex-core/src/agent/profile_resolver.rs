//! Резолюция профиля агента в цепочки провайдеров по capability.
//! Профиль не найден → Default + warn; нет и Default → пустые слоты
//! (агент без text-слота получает UnconfiguredProvider — существующий sentinel).

use crate::db::profiles::{Slots, SlotEntry, DEFAULT_PROFILE};
use sqlx::PgPool;

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
}
