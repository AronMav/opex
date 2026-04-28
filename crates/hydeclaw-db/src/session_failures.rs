//! Database operations for the structured session failure log.
//!
//! When a chat session terminates with `failed` status, the pipeline
//! records a row here capturing classified failure kind, the error message,
//! the last executed tool, provider/model, iteration count, and a
//! best-effort context blob. See migration `034_session_failures.sql`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Maximum bytes persisted in `last_tool_output`. Anything longer is
/// truncated on insert (UTF-8 boundary respected).
pub const MAX_TOOL_OUTPUT_BYTES: usize = 2048;

/// One row of the `session_failures` table.
#[derive(Debug, Clone, FromRow, serde::Serialize, serde::Deserialize)]
pub struct SessionFailureRecord {
    pub id: Uuid,
    pub session_id: Uuid,
    pub agent_id: String,
    pub failed_at: DateTime<Utc>,
    pub failure_kind: String,
    pub error_message: String,
    pub last_tool_name: Option<String>,
    pub last_tool_output: Option<String>,
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
    pub iteration_count: Option<i32>,
    pub duration_secs: Option<i32>,
    pub context_json: Option<serde_json::Value>,
}

/// Input payload for [`record_session_failure`]. All fields except
/// `session_id`, `agent_id`, `failure_kind`, and `error_message` are
/// optional — the recorder is best-effort.
#[derive(Debug, Clone)]
pub struct NewSessionFailure {
    pub session_id: Uuid,
    pub agent_id: String,
    pub failure_kind: String,
    pub error_message: String,
    pub last_tool_name: Option<String>,
    pub last_tool_output: Option<String>,
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
    pub iteration_count: Option<i32>,
    pub duration_secs: Option<i32>,
    pub context_json: Option<serde_json::Value>,
}

/// Truncate `s` to at most `MAX_TOOL_OUTPUT_BYTES` bytes, respecting UTF-8
/// codepoint boundaries.
pub fn truncate_tool_output(s: &str) -> &str {
    if s.len() <= MAX_TOOL_OUTPUT_BYTES {
        return s;
    }
    let mut end = MAX_TOOL_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Insert a new failure record. Returns the generated row id.
pub async fn record_session_failure(
    db: &PgPool,
    input: NewSessionFailure,
) -> Result<Uuid> {
    let truncated_output = input
        .last_tool_output
        .as_deref()
        .map(|s| truncate_tool_output(s).to_string());

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO session_failures ( \
             session_id, agent_id, failure_kind, error_message, \
             last_tool_name, last_tool_output, \
             llm_provider, llm_model, \
             iteration_count, duration_secs, context_json \
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         RETURNING id",
    )
    .bind(input.session_id)
    .bind(&input.agent_id)
    .bind(&input.failure_kind)
    .bind(&input.error_message)
    .bind(input.last_tool_name.as_deref())
    .bind(truncated_output.as_deref())
    .bind(input.llm_provider.as_deref())
    .bind(input.llm_model.as_deref())
    .bind(input.iteration_count)
    .bind(input.duration_secs)
    .bind(input.context_json.as_ref())
    .fetch_one(db)
    .await?;

    Ok(id)
}

/// List failures globally (or filtered by `agent_id`), most-recent first,
/// with pagination. `limit` is clamped to a sane upper bound by the caller.
pub async fn list_session_failures(
    db: &PgPool,
    agent_id: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<SessionFailureRecord>> {
    let rows = if let Some(agent) = agent_id {
        sqlx::query_as::<_, SessionFailureRecord>(
            "SELECT id, session_id, agent_id, failed_at, failure_kind, error_message, \
                    last_tool_name, last_tool_output, llm_provider, llm_model, \
                    iteration_count, duration_secs, context_json \
             FROM session_failures \
             WHERE agent_id = $1 \
             ORDER BY failed_at DESC \
             LIMIT $2 OFFSET $3",
        )
        .bind(agent)
        .bind(limit)
        .bind(offset)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query_as::<_, SessionFailureRecord>(
            "SELECT id, session_id, agent_id, failed_at, failure_kind, error_message, \
                    last_tool_name, last_tool_output, llm_provider, llm_model, \
                    iteration_count, duration_secs, context_json \
             FROM session_failures \
             ORDER BY failed_at DESC \
             LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(db)
        .await?
    };
    Ok(rows)
}

/// Total count, optionally filtered by `agent_id`. Used for pagination.
pub async fn count_session_failures(
    db: &PgPool,
    agent_id: Option<&str>,
) -> Result<i64> {
    let n: i64 = if let Some(agent) = agent_id {
        sqlx::query_scalar("SELECT COUNT(*)::BIGINT FROM session_failures WHERE agent_id = $1")
            .bind(agent)
            .fetch_one(db)
            .await?
    } else {
        sqlx::query_scalar("SELECT COUNT(*)::BIGINT FROM session_failures")
            .fetch_one(db)
            .await?
    };
    Ok(n)
}

/// Drill-down: all failures recorded for a single session, oldest first.
pub async fn get_session_failures_for_session(
    db: &PgPool,
    session_id: Uuid,
) -> Result<Vec<SessionFailureRecord>> {
    let rows = sqlx::query_as::<_, SessionFailureRecord>(
        "SELECT id, session_id, agent_id, failed_at, failure_kind, error_message, \
                last_tool_name, last_tool_output, llm_provider, llm_model, \
                iteration_count, duration_secs, context_json \
         FROM session_failures \
         WHERE session_id = $1 \
         ORDER BY failed_at ASC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_passthrough_short() {
        assert_eq!(truncate_tool_output("hello"), "hello");
    }

    #[test]
    fn truncate_caps_at_limit() {
        let s = "x".repeat(MAX_TOOL_OUTPUT_BYTES + 100);
        let out = truncate_tool_output(&s);
        assert_eq!(out.len(), MAX_TOOL_OUTPUT_BYTES);
    }

    #[test]
    fn truncate_respects_utf8_boundary() {
        // 4-byte glyph repeated past the cap; truncation must walk back to
        // a codepoint boundary.
        let glyph = "😀"; // 4 bytes
        let mut s = String::with_capacity(MAX_TOOL_OUTPUT_BYTES + 8);
        s.push('x'); // shift boundary so the cap mid-codepoint
        while s.len() < MAX_TOOL_OUTPUT_BYTES + 4 {
            s.push_str(glyph);
        }
        let out = truncate_tool_output(&s);
        assert!(out.len() <= MAX_TOOL_OUTPUT_BYTES);
        // Round-tripping confirms valid UTF-8.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}
