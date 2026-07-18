//! Tool quality tracking — penalty scores and call history for adaptive tool routing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// PenaltyCache
// ---------------------------------------------------------------------------

/// Nested penalty map: `agent_name → (tool_name → penalty)`.
type PenaltyMap = HashMap<String, HashMap<String, f32>>;

/// In-memory cache of tool penalty scores, refreshed from DB every 30 seconds.
/// Shared across all agents via Arc<PenaltyCache>.
///
/// The cached map is nested per-agent: `agent_name → (tool_name → penalty)`.
pub struct PenaltyCache {
    db: PgPool,
    /// (map, `last_refreshed_at`)
    cache: RwLock<(PenaltyMap, Instant)>,
}

impl PenaltyCache {
    /// Create a new cache. The timestamp is initialised 60 s in the past so the
    /// very first call to `get_penalties` triggers an immediate DB refresh.
    pub fn new(db: PgPool) -> Self {
        let stale = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or_else(Instant::now);
        Self {
            db,
            cache: RwLock::new((HashMap::new(), stale)),
        }
    }

    /// Returns a snapshot of penalty scores for the given agent's tools.
    /// Transparently refreshes from the DB when the cached data is older than 30 s.
    /// An agent with no tracked tools yields an empty submap.
    pub async fn get_penalties(&self, agent_name: &str) -> HashMap<String, f32> {
        {
            let guard = self.cache.read().await;
            if guard.1.elapsed() < Duration::from_secs(30) {
                return guard.0.get(agent_name).cloned().unwrap_or_default();
            }
        }

        // Upgrade to write lock and refresh.
        let mut guard = self.cache.write().await;
        // Double-checked locking: another task may have refreshed while we waited.
        if guard.1.elapsed() >= Duration::from_secs(30) {
            match get_all_penalties(&self.db).await {
                Ok(rows) => {
                    // Group (agent, tool) rows by agent into the nested map.
                    let mut nested = PenaltyMap::new();
                    for ((agent, tool), penalty) in rows {
                        nested.entry(agent).or_default().insert(tool, penalty);
                    }
                    guard.0 = nested;
                    guard.1 = Instant::now();
                }
                Err(e) => {
                    tracing::warn!("tool_quality: failed to refresh penalty cache: {e}");
                }
            }
        }
        guard.0.get(agent_name).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// record_tool_result
// ---------------------------------------------------------------------------

/// Upsert a tool call result into `tool_quality`.
///
/// * Increments total/success/fail counters and total latency.
/// * Appends an entry to `recent_calls` JSONB array (capped at last 20).
/// * Recalculates `penalty_score` from the recent window (floor: 0.2).
pub async fn record_tool_result(
    db: &PgPool,
    agent_name: &str,
    tool_name: &str,
    success: bool,
    duration_ms: i32,
    error: Option<&str>,
) -> Result<()> {
    let call_entry = json!({
        "success": success,
        "duration_ms": duration_ms,
        "error": error,
        "ts": chrono::Utc::now().to_rfc3339(),
    });

    // Bind order: $1=agent_name, $2=tool_name, $3=success, $4=duration_ms,
    // $5=call_entry (jsonb), $6=error. Penalty is scoped per (agent_name, tool_name).
    sqlx::query(
        r"
        INSERT INTO tool_quality (
            agent_name,
            tool_name,
            total_calls,
            success_calls,
            fail_calls,
            total_latency_ms,
            recent_calls,
            penalty_score,
            last_error,
            last_call_at,
            updated_at
        ) VALUES (
            $1,
            $2,
            1,
            CASE WHEN $3 THEN 1 ELSE 0 END,
            CASE WHEN $3 THEN 0 ELSE 1 END,
            $4,
            jsonb_build_array($5::jsonb),
            CASE WHEN $3 THEN 1.0 ELSE 0.2 END,
            $6,
            NOW(),
            NOW()
        )
        ON CONFLICT (agent_name, tool_name) DO UPDATE SET
            total_calls      = tool_quality.total_calls + 1,
            success_calls    = tool_quality.success_calls + CASE WHEN $3 THEN 1 ELSE 0 END,
            fail_calls       = tool_quality.fail_calls   + CASE WHEN $3 THEN 0 ELSE 1 END,
            total_latency_ms = tool_quality.total_latency_ms + $4,
            recent_calls     = (
                SELECT jsonb_agg(elem ORDER BY ordinality)
                FROM (
                    SELECT elem, ordinality
                    FROM jsonb_array_elements(
                        tool_quality.recent_calls || jsonb_build_array($5::jsonb)
                    ) WITH ORDINALITY AS t(elem, ordinality)
                    ORDER BY ordinality DESC
                    LIMIT 20
                ) sub
            ),
            penalty_score    = GREATEST(
                0.2,
                (
                    SELECT COALESCE(
                        AVG(CASE WHEN (elem->>'success')::boolean THEN 1.0 ELSE 0.0 END),
                        1.0
                    )
                    FROM (
                        SELECT elem
                        FROM jsonb_array_elements(
                            tool_quality.recent_calls || jsonb_build_array($5::jsonb)
                        ) WITH ORDINALITY AS t(elem, ordinality)
                        ORDER BY ordinality DESC
                        LIMIT 20
                    ) window_sub
                )
            ),
            last_error       = CASE WHEN $3 THEN tool_quality.last_error ELSE $6 END,
            last_call_at     = NOW(),
            updated_at       = NOW()
        ",
    )
    .bind(agent_name)
    .bind(tool_name)
    .bind(success)
    .bind(i64::from(duration_ms))
    .bind(&call_entry)
    .bind(error)
    .execute(db)
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// get_all_penalties  (private)
// ---------------------------------------------------------------------------

async fn get_all_penalties(db: &PgPool) -> Result<HashMap<(String, String), f32>> {
    let rows: Vec<(String, String, f32)> =
        sqlx::query_as("SELECT agent_name, tool_name, penalty_score FROM tool_quality")
            .fetch_all(db)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(agent, tool, penalty)| ((agent, tool), penalty))
        .collect())
}

// ---------------------------------------------------------------------------
// get_degraded_tools
// ---------------------------------------------------------------------------

/// Returns tools whose `penalty_score` is below 0.8.
/// Used by `GET /api/doctor` to surface degraded tools.
pub async fn get_degraded_tools(db: &PgPool) -> Result<Vec<serde_json::Value>> {
    // total_calls / fail_calls are INT4 (i32); penalty_score is REAL (f32).
    // (Was i64 — latent decode error whenever a degraded tool existed.)
    let rows = sqlx::query_as::<_, (String, String, f32, i32, i32, Option<String>)>(
        r"
        SELECT agent_name, tool_name, penalty_score, total_calls, fail_calls, last_error
        FROM tool_quality
        WHERE penalty_score < 0.8
        ORDER BY penalty_score ASC
        ",
    )
    .fetch_all(db)
    .await?;

    let result = rows
        .into_iter()
        .map(|(agent, name, penalty, total, fail, last_error)| {
            json!({
                "agent_name": agent,
                "tool_name": name,
                "penalty_score": penalty,
                "total_calls": total,
                "fail_calls": fail,
                "last_error": last_error,
            })
        })
        .collect();

    Ok(result)
}

// ---------------------------------------------------------------------------
// get_tool_health
// ---------------------------------------------------------------------------

/// Failing tools ordered by impact (fail-share × fail-volume), worst first.
/// Complements `get_degraded_tools` (penalty<0.8 → `/api/doctor`) with the raw
/// counters an operator needs to see WHAT is failing and how often. Feeds
/// `GET /api/tools/health`.
pub async fn get_tool_health(db: &PgPool) -> Result<Vec<serde_json::Value>> {
    // total_calls / fail_calls are INT4 (i32); penalty_score is REAL (f32).
    let rows = sqlx::query_as::<_, (String, String, f32, i32, i32, Option<String>)>(
        r"
        SELECT agent_name, tool_name, penalty_score, total_calls, fail_calls, last_error
        FROM tool_quality
        WHERE fail_calls > 0
        ORDER BY (fail_calls::float / NULLIF(total_calls, 0)) * fail_calls DESC
        ",
    )
    .fetch_all(db)
    .await?;

    let result = rows
        .into_iter()
        .map(|(agent, name, penalty, total, fail, last_error)| {
            json!({
                "agent_name": agent,
                "tool_name": name,
                "penalty_score": penalty,
                "total_calls": total,
                "fail_calls": fail,
                "last_error": last_error,
            })
        })
        .collect();

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn tool_health_orders_by_impact_and_excludes_healthy(pool: sqlx::PgPool) {
        // high impact: 2 fails / 2 total → fail-share 1.0 × 2 fails = 2.0
        record_tool_result(&pool, "A", "bad_tool", false, 100, Some("boom")).await.unwrap();
        record_tool_result(&pool, "A", "bad_tool", false, 100, Some("boom")).await.unwrap();
        // low impact: 1 fail / 5 total → fail-share 0.2 × 1 fail = 0.2
        for _ in 0..4 {
            record_tool_result(&pool, "A", "meh_tool", true, 10, None).await.unwrap();
        }
        record_tool_result(&pool, "A", "meh_tool", false, 10, Some("x")).await.unwrap();
        // healthy: no fails → excluded
        record_tool_result(&pool, "A", "good_tool", true, 50, None).await.unwrap();

        let rows = get_tool_health(&pool).await.unwrap();
        assert_eq!(rows.len(), 2, "only tools with fail_calls > 0");
        assert_eq!(rows[0]["tool_name"], "bad_tool", "highest-impact tool first");
        assert_eq!(rows[0]["fail_calls"], 2);
        assert_eq!(rows[1]["tool_name"], "meh_tool", "lower-impact tool second");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn penalty_is_scoped_per_agent(pool: sqlx::PgPool) {
        // Tool "T" fails repeatedly under agent A, succeeds under agent B.
        for _ in 0..5 {
            record_tool_result(&pool, "A", "T", false, 10, Some("boom")).await.unwrap();
        }
        for _ in 0..5 {
            record_tool_result(&pool, "B", "T", true, 10, None).await.unwrap();
        }

        let all = get_all_penalties(&pool).await.unwrap();
        assert!(all[&("A".to_string(), "T".to_string())] < 0.8, "A's T is penalized");
        assert!(
            (all[&("B".to_string(), "T".to_string())] - 1.0).abs() < f32::EPSILON,
            "B's T is clean"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn penalty_cache_returns_per_agent_submap(pool: sqlx::PgPool) {
        for _ in 0..5 {
            record_tool_result(&pool, "A", "T", false, 10, Some("x")).await.unwrap();
        }
        let cache = PenaltyCache::new(pool.clone());
        let a = cache.get_penalties("A").await;
        let b = cache.get_penalties("B").await;
        assert!(a.get("T").copied().unwrap_or(1.0) < 0.8, "A sees its penalty");
        assert!(!b.contains_key("T"), "B (unseen) gets an empty submap");
    }
}
