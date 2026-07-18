//! Soul retrieval scoring (spec §1): score sums normalized recency, importance,
//! and relevance with Stanford weights 1/1/1. recency = 0.995^hours(created_at)
//! — from created_at, NOT accessed_at (write-on-read disabled adversarially).
//! Diversification: ≤3 EVENTS per source session; reflections exempt (they all
//! share source='soul_reflection' — naive grouping would cap them at 3 total).

use chrono::{DateTime, Utc};
use crate::memory::SoulCandidate;

pub(crate) const RECENCY_DECAY: f64 = 0.995;
pub(crate) const PER_SESSION_DIVERSITY_CAP: usize = 3;
pub(crate) const SOUL_CANDIDATE_LIMIT: i64 = 50;
/// Reflection floor for the candidate pool. Events vastly outnumber reflections
/// (~473:27 observed), so a pure top-N ANN over both kinds crowds reflections
/// out of the pool entirely and the durable (reflection) layer never reaches
/// scoring. Pull the top reflections separately and union them in, so
/// `score_and_select` always sees them (spec §retrieval quota).
pub(crate) const SOUL_REFLECTION_FLOOR: i64 = 15;

/// One item for transactional soul indexing (reflection cycle step 4).
#[derive(Debug, Clone)]
pub struct SoulInsert {
    pub content: String,
    pub source: String,
    pub kind: String,
    pub importance: f32,
    pub lineage: Option<Vec<uuid::Uuid>>,
}

fn minmax(values: &[f64]) -> Vec<f64> {
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || !max.is_finite() || (max - min).abs() < f64::EPSILON {
        // Degenerate spread: the component cannot discriminate — contribute
        // equally (1.0) so the other two components decide.
        return vec![1.0; values.len()];
    }
    values.iter().map(|v| (v - min) / (max - min)).collect()
}

pub fn score_and_select(
    cands: Vec<SoulCandidate>,
    now: DateTime<Utc>,
    top_k: usize,
) -> Vec<SoulCandidate> {
    if cands.is_empty() {
        return vec![];
    }
    let recency: Vec<f64> = cands.iter()
        .map(|c| {
            let hours = (now - c.created_at).num_seconds().max(0) as f64 / 3600.0;
            RECENCY_DECAY.powf(hours)
        })
        .collect();
    let importance: Vec<f64> = cands.iter().map(|c| f64::from(c.importance) / 10.0).collect();
    let relevance: Vec<f64> = cands.iter().map(|c| c.similarity).collect();

    let (r, i, s) = (minmax(&recency), minmax(&importance), minmax(&relevance));
    let mut scored: Vec<(f64, SoulCandidate)> = cands.into_iter().enumerate()
        .map(|(idx, c)| (r[idx] + i[idx] + s[idx], c))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.id.cmp(&b.1.id))
    });

    let mut per_source: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(top_k);
    for (_, c) in scored {
        if c.kind == "event" {
            let n = per_source.entry(c.source.clone()).or_insert(0);
            if *n >= PER_SESSION_DIVERSITY_CAP {
                continue;
            }
            *n += 1;
        }
        out.push(c);
        if out.len() >= top_k {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn cand(source: &str, kind: &str, importance: f32, hours_ago: i64, sim: f64) -> crate::memory::SoulCandidate {
        crate::memory::SoulCandidate {
            id: uuid::Uuid::new_v4(),
            content: format!("c-{source}-{sim}"),
            source: source.to_string(),
            kind: kind.to_string(),
            importance,
            created_at: Utc::now() - Duration::hours(hours_ago),
            similarity: sim,
        }
    }

    #[test]
    fn importance_dominates_when_recency_and_relevance_equal() {
        let now = Utc::now();
        let hi = cand("soul_event:a", "event", 10.0, 5, 0.5);
        let lo = cand("soul_event:b", "event", 1.0, 5, 0.5);
        let out = score_and_select(vec![lo, hi.clone()], now, 1);
        assert_eq!(out[0].id, hi.id);
    }

    #[test]
    fn recency_dominates_when_importance_and_relevance_equal() {
        let now = Utc::now();
        let fresh = cand("soul_event:a", "event", 5.0, 1, 0.5);
        let old = cand("soul_event:b", "event", 5.0, 24 * 90, 0.5);
        let out = score_and_select(vec![old, fresh.clone()], now, 1);
        assert_eq!(out[0].id, fresh.id);
    }

    #[test]
    fn single_candidate_survives_minmax() {
        let now = Utc::now();
        let only = cand("soul_event:a", "event", 5.0, 5, 0.5);
        let out = score_and_select(vec![only.clone()], now, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, only.id);
    }

    #[test]
    fn degenerate_spread_all_components_equal_returns_all() {
        // Все три компоненты одинаковы у всех кандидатов → minmax вырожден
        // (max==min) → компоненты дают константу, отбор не паникует и не пуст.
        let now = Utc::now();
        let cands: Vec<_> = (0..4)
            .map(|i| cand(&format!("soul_event:{i}"), "event", 5.0, 5, 0.5))
            .collect();
        let out = score_and_select(cands, now, 4);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn diversity_caps_events_per_session_but_not_reflections() {
        let now = Utc::now();
        let mut cands = Vec::new();
        // 5 top-scoring events from the same session
        for i in 0..5 {
            cands.push(cand("soul_event:same", "event", 10.0, 1, 0.9 - i as f64 * 0.01));
        }
        // 5 reflections (shared source soul_reflection) — must NOT be capped
        for i in 0..5 {
            cands.push(cand("soul_reflection", "reflection", 9.0, 1, 0.8 - i as f64 * 0.01));
        }
        let out = score_and_select(cands, now, 10);
        let same_events = out.iter().filter(|c| c.source == "soul_event:same").count();
        let reflections = out.iter().filter(|c| c.kind == "reflection").count();
        assert_eq!(same_events, 3, "events per session capped at 3");
        assert_eq!(reflections, 5, "reflections exempt from per-source cap");
    }
}
