//! Mood persistence for the emotion layer (spec
//! `docs/superpowers/specs/2026-07-14-agent-soul-emotion-layer-v1-design.md` +
//! render-v2 §3.4). `upsert_blended` is called from the knowledge extractor's
//! appraisal piggyback; `get` is read by the emotion prompt-render v2 path
//! (`AgentEngine::emotion_mood_block`).

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::agent::emotion::{blend, decay};
use crate::config::EmotionConfig;

#[derive(Debug, Clone)]
pub struct MoodRow {
    pub valence: f32,
    pub label: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Current stored mood (raw, not decayed). Callers that render/consume it apply
/// `emotion::decay` by elapsed-since-`updated_at` themselves. Read by the
/// emotion prompt-render v2 path (`AgentEngine::emotion_mood_block`).
pub async fn get(db: &PgPool, agent_id: &str) -> Result<Option<MoodRow>> {
    let row = sqlx::query_as::<_, (f32, Option<String>, DateTime<Utc>)>(
        "SELECT valence, label, updated_at FROM agent_emotion_state WHERE agent_id = $1",
    )
    .bind(agent_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(valence, label, updated_at)| MoodRow { valence, label, updated_at }))
}

/// Read-decay-blend-write in one FOR UPDATE transaction (closes the RMW race
/// between two near-simultaneous session finishes for the same agent).
pub async fn upsert_blended(
    db: &PgPool,
    agent_id: &str,
    new_valence: f32,
    label: Option<&str>,
    intensity: f32,
    cfg: &EmotionConfig,
) -> Result<f32> {
    let mut tx = db.begin().await?;
    let existing = sqlx::query_as::<_, (f32, DateTime<Utc>)>(
        "SELECT valence, updated_at FROM agent_emotion_state WHERE agent_id = $1 FOR UPDATE",
    )
    .bind(agent_id)
    .fetch_optional(&mut *tx)
    .await?;

    let decayed = match existing {
        Some((valence, updated_at)) => {
            let elapsed_hours = (Utc::now() - updated_at).num_seconds() as f32 / 3600.0;
            decay(valence, elapsed_hours, cfg.decay_half_life_hours)
        }
        None => 0.0,
    };
    let blended = blend(decayed, new_valence, cfg.blend_rate, intensity);

    sqlx::query(
        "INSERT INTO agent_emotion_state (agent_id, valence, label, updated_at) \
         VALUES ($1, $2, $3, now()) \
         ON CONFLICT (agent_id) DO UPDATE SET valence = $2, label = $3, updated_at = now()",
    )
    .bind(agent_id)
    .bind(blended)
    .bind(label)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    // Return the post-blend mood valence so callers can record `mood_valence_after`
    // for observability without a follow-up read.
    Ok(blended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EmotionConfig;

    #[sqlx::test(migrations = "../../migrations")]
    async fn upsert_then_get_roundtrip_and_blend(pool: sqlx::PgPool) -> sqlx::Result<()> {
        let cfg = EmotionConfig { blend_rate: 0.5, decay_half_life_hours: 12.0, ..Default::default() };
        // fresh agent: no row → baseline 0, blend toward +1 at intensity 1 → +0.5
        upsert_blended(&pool, "EM", 1.0, Some("радость"), 1.0, &cfg).await.unwrap();
        let m = get(&pool, "EM").await.unwrap().unwrap();
        assert!((m.valence - 0.5).abs() < 1e-3, "got {}", m.valence);
        assert_eq!(m.label.as_deref(), Some("радость"));
        // second upsert toward -1 (same tick → ~no decay): 0.5*(0.5)+(-1)*0.5 = -0.25
        upsert_blended(&pool, "EM", -1.0, Some("грусть"), 1.0, &cfg).await.unwrap();
        let m2 = get(&pool, "EM").await.unwrap().unwrap();
        assert!(m2.valence < 0.2 && m2.valence > -0.5, "blended toward negative, got {}", m2.valence);
        assert_eq!(m2.label.as_deref(), Some("грусть"));
        Ok(())
    }
}
