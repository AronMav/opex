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

/// In-memory cache of tool penalty scores, refreshed from DB every 30 seconds.
/// Shared across all agents via Arc<PenaltyCache>.
pub struct PenaltyCache {
    db: PgPool,
    /// (map, `last_refreshed_at`)
    cache: RwLock<(HashMap<String, f32>, Instant)>,
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

    /// Returns a snapshot of penalty scores for all tracked tools.
    /// Transparently refreshes from the DB when the cached data is older than 30 s.
    pub async fn get_penalties(&self) -> HashMap<String, f32> {
        {
            let guard = self.cache.read().await;
            if guard.1.elapsed() < Duration::from_secs(30) {
                return guard.0.clone();
            }
        }

        // Upgrade to write lock and refresh.
        let mut guard = self.cache.write().await;
        // Double-checked locking: another task may have refreshed while we waited.
        if guard.1.elapsed() >= Duration::from_secs(30) {
            match get_all_penalties(&self.db).await {
                Ok(map) => {
                    guard.0 = map;
                    guard.1 = Instant::now();
                }
                Err(e) => {
                    tracing::warn!("tool_quality: failed to refresh penalty cache: {e}");
                }
            }
        }
        guard.0.clone()
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

    sqlx::query(
        r"
        INSERT INTO tool_quality (
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
            1,
            CASE WHEN $2 THEN 1 ELSE 0 END,
            CASE WHEN $2 THEN 0 ELSE 1 END,
            $3,
            jsonb_build_array($4::jsonb),
            CASE WHEN $2 THEN 1.0 ELSE 0.2 END,
            $5,
            NOW(),
            NOW()
        )
        ON CONFLICT (tool_name) DO UPDATE SET
            total_calls      = tool_quality.total_calls + 1,
            success_calls    = tool_quality.success_calls + CASE WHEN $2 THEN 1 ELSE 0 END,
            fail_calls       = tool_quality.fail_calls   + CASE WHEN $2 THEN 0 ELSE 1 END,
            total_latency_ms = tool_quality.total_latency_ms + $3,
            recent_calls     = (
                SELECT jsonb_agg(elem ORDER BY ordinality)
                FROM (
                    SELECT elem, ordinality
                    FROM jsonb_array_elements(
                        tool_quality.recent_calls || jsonb_build_array($4::jsonb)
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
                            tool_quality.recent_calls || jsonb_build_array($4::jsonb)
                        ) WITH ORDINALITY AS t(elem, ordinality)
                        ORDER BY ordinality DESC
                        LIMIT 20
                    ) window_sub
                )
            ),
            last_error       = CASE WHEN $2 THEN tool_quality.last_error ELSE $5 END,
            last_call_at     = NOW(),
            updated_at       = NOW()
        ",
    )
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

async fn get_all_penalties(db: &PgPool) -> Result<HashMap<String, f32>> {
    let rows: Vec<(String, f32)> =
        sqlx::query_as("SELECT tool_name, penalty_score FROM tool_quality")
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().collect())
}

// ---------------------------------------------------------------------------
// get_degraded_tools
// ---------------------------------------------------------------------------

/// Returns tools whose `penalty_score` is below 0.8.
/// Used by `GET /api/doctor` to surface degraded tools.
pub async fn get_degraded_tools(db: &PgPool) -> Result<Vec<serde_json::Value>> {
    let rows = sqlx::query_as::<_, (String, f32, i64, i64, Option<String>)>(
        r"
        SELECT tool_name, penalty_score, total_calls, fail_calls, last_error
        FROM tool_quality
        WHERE penalty_score < 0.8
        ORDER BY penalty_score ASC
        ",
    )
    .fetch_all(db)
    .await?;

    let result = rows
        .into_iter()
        .map(|(name, penalty, total, fail, last_error)| {
            json!({
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
