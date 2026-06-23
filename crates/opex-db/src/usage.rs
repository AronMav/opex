use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Status value for calls aborted without failover retry (max_duration,
/// user_cancelled, shutdown_drain). Persisted to `usage_log.status`.
/// Changing this string breaks migration 025's documented enum.
pub const STATUS_ABORTED: &str = "aborted";

/// Status value for calls aborted WITH failover to a sibling provider.
/// Partial content was produced before the failover occurred.
pub const STATUS_ABORTED_FAILOVER: &str = "aborted_failover";

/// Record a single LLM call's token usage.
///
/// Extended fields on `TokenUsage` (`cache_read_tokens`, `cache_creation_tokens`,
/// `reasoning_tokens`) are SUBSETS of `input_tokens` (cache_*) and
/// `output_tokens` (reasoning). They are `None` when the provider does not
/// return them. Never sum.
///
/// Takes `&TokenUsage` rather than 5 expanded fields so a future `TokenUsage`
/// extension (e.g., audio/image tokens) doesn't ripple through every call site
/// and so the compiler catches `(cache_read, cache_creation)` field swaps that
/// `Option<u32>` positional arguments cannot.
pub async fn record_usage(
    db: &PgPool,
    agent_id: &str,
    provider: &str,
    model: &str,
    session_id: Option<Uuid>,
    usage: &opex_types::TokenUsage,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage_log (\
            agent_id, provider, model, \
            input_tokens, output_tokens, \
            session_id, \
            cache_read_tokens, cache_creation_tokens, reasoning_tokens) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(agent_id)
    .bind(provider)
    .bind(model)
    .bind(usage.input_tokens as i32)
    .bind(usage.output_tokens as i32)
    .bind(session_id)
    .bind(usage.cache_read_tokens.map(|v| v as i32))
    .bind(usage.cache_creation_tokens.map(|v| v as i32))
    .bind(usage.reasoning_tokens.map(|v| v as i32))
    .execute(db)
    .await?;
    Ok(())
}

/// Insert a usage_log row marked as aborted (with or without failover).
///
/// Pure SQL helper — caller supplies the already-decided `status` string
/// (use [`STATUS_ABORTED`] / [`STATUS_ABORTED_FAILOVER`]). Keeping this as a
/// pure SQL helper means the DB contract can be integration-tested without
/// pulling the engine's `LlmCallError` downcast logic into the lib facade.
///
/// `input_tokens` is always written as `0` for aborted calls (we don't
/// know the prompt size until the usage headers arrive, which aborts
/// by definition don't get). `output_tokens` is the caller's estimate —
/// typically `partial_text.len() / 4` as a rough bytes-per-token
/// heuristic.
///
/// Note: aborted rows always have NULL for cache_read/cache_creation/reasoning_tokens
/// columns — provider headers never arrived, so extended usage info is unavailable.
/// Schema's NULL is the correct representation; do NOT add Some(0).
pub async fn insert_aborted_row(
    db: &PgPool,
    agent_id: &str,
    provider: &str,
    model: &str,
    session_id: Uuid,
    output_tokens: u32,
    status: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage_log (agent_id, provider, model, input_tokens, output_tokens, session_id, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(agent_id)
    .bind(provider)
    .bind(model)
    .bind(0_i32)
    .bind(output_tokens as i32)
    .bind(session_id)
    .bind(status)
    .execute(db)
    .await?;
    Ok(())
}

/// Get total tokens used by an agent today.
pub async fn get_agent_usage_today(db: &PgPool, agent_id: &str) -> Result<i64> {
    let total: (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM usage_log \
         WHERE agent_id = $1 AND created_at > CURRENT_DATE",
    )
    .bind(agent_id)
    .fetch_one(db)
    .await?;
    Ok(total.0)
}

/// Usage summary for an agent over a time period.
#[derive(Debug, serde::Serialize)]
pub struct UsageSummary {
    pub agent_id: String,
    pub provider: String,
    pub model: String,
    pub total_input: i64,
    pub total_output: i64,
    pub call_count: i64,
    pub estimated_cost: Option<f64>,
}

/// Estimate cost in USD based on provider/model pricing (per 1M tokens).
fn estimate_cost(provider: &str, model: &str, input: i64, output: i64) -> Option<f64> {
    let (input_per_m, output_per_m) = match (provider, model) {
        ("minimax", _) if model.contains("M2.5") => (0.50, 1.50),
        ("minimax", _) => (0.50, 1.50),
        ("anthropic", m) if m.contains("opus") => (15.00, 75.00),
        ("anthropic", m) if m.contains("sonnet") => (3.00, 15.00),
        ("anthropic", m) if m.contains("haiku") => (0.25, 1.25),
        ("anthropic", _) => (3.00, 15.00),
        ("openai", m) if m.contains("gpt-4o-mini") => (0.15, 0.60),
        ("openai", m) if m.contains("gpt-4o") => (2.50, 10.00),
        ("openai", _) => (2.50, 10.00),
        ("google", m) if m.contains("flash") => (0.10, 0.40),
        ("google", m) if m.contains("pro") => (1.25, 5.00),
        ("google", _) => (0.10, 0.40),
        ("deepseek", _) => (0.14, 0.28),
        ("groq", _) => (0.05, 0.08),
        ("xai", _) => (2.00, 10.00),
        ("together", _) => (0.20, 0.60),
        ("openrouter", _) => (0.50, 1.50),
        ("mistral", _) => (0.30, 0.90),
        ("perplexity", _) => (1.00, 5.00),
        ("ollama", _) => (0.0, 0.0),
        _ => return None,
    };
    let cost = (input as f64 / 1_000_000.0) * input_per_m
        + (output as f64 / 1_000_000.0) * output_per_m;
    Some((cost * 10000.0).round() / 10000.0) // 4 decimal places
}

/// Daily usage breakdown for charts.
#[derive(Debug, serde::Serialize)]
pub struct DailyUsage {
    pub date: String,
    pub agent_id: String,
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub call_count: i64,
}

/// Get daily usage breakdown for the last N days.
pub async fn usage_daily(db: &PgPool, days: u32) -> Result<Vec<DailyUsage>> {
    let rows = sqlx::query_as::<_, (chrono::NaiveDate, String, String, String, i64, i64, i64)>(
        "SELECT date_trunc('day', created_at)::date as day, \
         agent_id, provider, COALESCE(model, ''), \
         COALESCE(SUM(input_tokens), 0), \
         COALESCE(SUM(output_tokens), 0), \
         COUNT(*) \
         FROM usage_log \
         WHERE created_at > now() - make_interval(days => $1) \
         GROUP BY day, agent_id, provider, COALESCE(model, '') \
         ORDER BY day",
    )
    .bind(days as i32)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(date, agent_id, provider, model, input_tokens, output_tokens, call_count)| {
            DailyUsage {
                date: date.to_string(),
                agent_id,
                provider,
                model,
                input_tokens,
                output_tokens,
                call_count,
            }
        })
        .collect())
}

/// Per-session usage breakdown.
#[derive(Debug, serde::Serialize)]
pub struct SessionUsage {
    pub session_id: Uuid,
    pub agent_id: String,
    pub total_input: i64,
    pub total_output: i64,
    pub call_count: i64,
    pub estimated_cost: Option<f64>,
}

/// Get usage grouped by session for the last N days.
pub async fn usage_by_session(db: &PgPool, agent_id: Option<&str>, days: u32) -> Result<Vec<SessionUsage>> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, i64, i64, i64)>(
        "SELECT session_id, agent_id, provider, COALESCE(model, ''), \
         COALESCE(SUM(input_tokens), 0), \
         COALESCE(SUM(output_tokens), 0), \
         COUNT(*) \
         FROM usage_log \
         WHERE session_id IS NOT NULL \
         AND created_at > now() - make_interval(days => $1) \
         AND ($2::TEXT IS NULL OR agent_id = $2) \
         GROUP BY session_id, agent_id, provider, COALESCE(model, '') \
         ORDER BY SUM(input_tokens) + SUM(output_tokens) DESC \
         LIMIT 200",
    )
    .bind(days as i32)
    .bind(agent_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(session_id, agent_id, provider, model, total_input, total_output, call_count)| {
            let estimated_cost = estimate_cost(&provider, &model, total_input, total_output);
            SessionUsage {
                session_id,
                agent_id,
                total_input,
                total_output,
                call_count,
                estimated_cost,
            }
        })
        .collect())
}

/// Get usage summary grouped by agent+provider+model for the last N days.
pub async fn usage_summary(db: &PgPool, days: u32) -> Result<Vec<UsageSummary>> {
    let rows = sqlx::query_as::<_, (String, String, String, i64, i64, i64)>(
        "SELECT agent_id, provider, COALESCE(model, ''), \
         COALESCE(SUM(input_tokens), 0), \
         COALESCE(SUM(output_tokens), 0), \
         COUNT(*) \
         FROM usage_log \
         WHERE created_at > now() - make_interval(days => $1) \
         GROUP BY agent_id, provider, COALESCE(model, '') \
         ORDER BY agent_id, provider, COALESCE(model, '')",
    )
    .bind(days as i32)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(agent_id, provider, model, total_input, total_output, call_count)| {
            let estimated_cost = estimate_cost(&provider, &model, total_input, total_output);
            UsageSummary {
                agent_id,
                provider,
                model,
                total_input,
                total_output,
                call_count,
                estimated_cost,
            }
        })
        .collect())
}

/// Aggregated cache token totals from `usage_log` over rolling time windows.
///
/// All four fields are `i64` because `SUM(INTEGER)` in PostgreSQL returns
/// `BIGINT` and `COALESCE(..., 0)` keeps the type as BIGINT. Token counts
/// are non-negative; the signed type is for decode compatibility, not range.
///
/// `Default::default()` returns all zeros — used by the dashboard handler
/// to degrade gracefully on DB errors rather than failing the entire
/// dashboard request.
#[derive(Debug, Clone, Default)]
pub struct CacheMetrics {
    pub cache_read_tokens_24h: i64,
    pub cache_creation_tokens_24h: i64,
    pub cache_read_tokens_7d: i64,
    pub cache_creation_tokens_7d: i64,
}

/// Cache token aggregates for the last 24 hours and 7 days, suitable for
/// `/api/health/dashboard`. CACHE-03.
///
/// Both `cache_read_tokens` and `cache_creation_tokens` are nullable in
/// `usage_log` (aborted rows write NULL — see [`insert_aborted_row`]).
/// `SUM` ignores NULL; `COALESCE(SUM(...), 0)` ensures an empty result set
/// returns 0 instead of NULL.
///
/// Pitfall 6 mitigation: queries use `WHERE created_at > now() - interval`
/// on the indexed `created_at` column (short windows; bounded scan).
pub async fn cache_metrics(db: &PgPool) -> Result<CacheMetrics> {
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           COALESCE(SUM(cache_read_tokens) FILTER (WHERE created_at > now() - interval '24 hours'), 0)::BIGINT, \
           COALESCE(SUM(cache_creation_tokens) FILTER (WHERE created_at > now() - interval '24 hours'), 0)::BIGINT, \
           COALESCE(SUM(cache_read_tokens) FILTER (WHERE created_at > now() - interval '7 days'), 0)::BIGINT, \
           COALESCE(SUM(cache_creation_tokens) FILTER (WHERE created_at > now() - interval '7 days'), 0)::BIGINT \
         FROM usage_log \
         WHERE created_at > now() - interval '7 days'",
    )
    .fetch_one(db)
    .await?;
    Ok(CacheMetrics {
        cache_read_tokens_24h: row.0,
        cache_creation_tokens_24h: row.1,
        cache_read_tokens_7d: row.2,
        cache_creation_tokens_7d: row.3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimax_m25_pricing() {
        // 1M input @ $0.50/M + 1M output @ $1.50/M = $2.00
        let cost = estimate_cost("minimax", "MiniMax-M2.5", 1_000_000, 1_000_000);
        assert_eq!(cost, Some(2.0));
    }

    #[test]
    fn anthropic_opus_pricing() {
        // 1M input @ $15.00/M + 1M output @ $75.00/M = $90.00
        let cost = estimate_cost("anthropic", "claude-opus-3", 1_000_000, 1_000_000);
        assert_eq!(cost, Some(90.0));
    }

    #[test]
    fn anthropic_sonnet_pricing() {
        // 1M input @ $3.00/M + 1M output @ $15.00/M = $18.00
        let cost = estimate_cost("anthropic", "claude-sonnet-4", 1_000_000, 1_000_000);
        assert_eq!(cost, Some(18.0));
    }

    #[test]
    fn openai_gpt4o_mini_pricing() {
        // 1M input @ $0.15/M + 1M output @ $0.60/M = $0.75
        let cost = estimate_cost("openai", "gpt-4o-mini", 1_000_000, 1_000_000);
        assert_eq!(cost, Some(0.75));
    }

    #[test]
    fn openai_gpt4o_pricing() {
        // 1M input @ $2.50/M + 1M output @ $10.00/M = $12.50
        let cost = estimate_cost("openai", "gpt-4o", 1_000_000, 1_000_000);
        assert_eq!(cost, Some(12.5));
    }

    #[test]
    fn unknown_provider_returns_none() {
        let cost = estimate_cost("unknownprovider", "some-model", 1_000_000, 1_000_000);
        assert_eq!(cost, None);
    }

    #[test]
    fn zero_tokens_returns_zero_cost() {
        let cost = estimate_cost("anthropic", "claude-sonnet-4", 0, 0);
        assert_eq!(cost, Some(0.0));
    }

    #[test]
    fn ollama_is_free() {
        let cost = estimate_cost("ollama", "llama3", 1_000_000, 1_000_000);
        assert_eq!(cost, Some(0.0));
    }

    #[test]
    fn one_million_input_one_million_output_deepseek() {
        // deepseek: $0.14/M input + $0.28/M output = $0.42
        let cost = estimate_cost("deepseek", "deepseek-chat", 1_000_000, 1_000_000);
        assert_eq!(cost, Some(0.42));
    }

    #[test]
    fn aborted_status_constants_pinned() {
        // Changing these strings requires migration 025 to be amended.
        assert_eq!(STATUS_ABORTED, "aborted");
        assert_eq!(STATUS_ABORTED_FAILOVER, "aborted_failover");
    }

    #[test]
    fn cache_metrics_default_is_all_zeros() {
        // Default impl is what the dashboard handler uses on DB error,
        // so it must be safe to surface as JSON without surprise.
        let m = CacheMetrics::default();
        assert_eq!(m.cache_read_tokens_24h, 0);
        assert_eq!(m.cache_creation_tokens_24h, 0);
        assert_eq!(m.cache_read_tokens_7d, 0);
        assert_eq!(m.cache_creation_tokens_7d, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cache_metrics_returns_zeros_on_empty_table(pool: PgPool) {
        // CACHE-03: COALESCE-to-zero handling proven on empty table.
        let m = cache_metrics(&pool).await.expect("query succeeds on empty table");
        assert_eq!(m.cache_read_tokens_24h, 0);
        assert_eq!(m.cache_creation_tokens_24h, 0);
        assert_eq!(m.cache_read_tokens_7d, 0);
        assert_eq!(m.cache_creation_tokens_7d, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cache_metrics_sums_recent_rows(pool: PgPool) {
        // Insert two rows: one within the 24h window (default created_at = now()),
        // one outside (10 days old). Verify 24h and 7d aggregates exclude the
        // older row (since 10d > 7d > 24h).
        sqlx::query(
            "INSERT INTO usage_log (agent_id, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens) \
             VALUES ('a', 'p', 'm', 100, 50, 700, 200)",
        )
        .execute(&pool)
        .await
        .expect("insert recent");
        sqlx::query(
            "INSERT INTO usage_log (agent_id, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, created_at) \
             VALUES ('a', 'p', 'm', 100, 50, 9999, 9999, now() - interval '10 days')",
        )
        .execute(&pool)
        .await
        .expect("insert old");

        let m = cache_metrics(&pool).await.expect("query succeeds");
        assert_eq!(m.cache_read_tokens_24h, 700, "24h must include only recent");
        assert_eq!(m.cache_creation_tokens_24h, 200);
        assert_eq!(m.cache_read_tokens_7d, 700, "7d must also exclude 10d-old row");
        assert_eq!(m.cache_creation_tokens_7d, 200);
    }
}
