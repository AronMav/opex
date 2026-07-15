//! Streaming-message DB helpers used by `sse_converter.rs` (the converter
//! task spawned from `sse.rs`'s POST handler) to persist the streaming
//! assistant text into the `messages` row as it accumulates. GET
//! `/api/chat/{id}/stream` (`stream.rs`) does NOT read this row — it
//! replays from `StreamRegistry`/`stream_jobs` instead (the old `resume.rs`
//! consumer of this row schema no longer exists; renamed/rewritten as
//! `stream.rs` in T4).
//!
//! - [`StreamingMessageGuard`]: RAII finalizer that schedules a `DELETE` of
//!   the streaming row if the converter task panics or exits unexpectedly.
//! - [`build_tools_json`]: cache-aware `Vec<Value> → Value` builder for
//!   periodic `tool_calls` flushes (avoids redundant `to_vec()`).
//! - [`upsert_streaming_append`]: append-mode upsert that pins
//!   `parent_message_id` to the most-recent user row on first INSERT.
//! - [`read_streaming_content`]: reads the aggregated body before the row is
//!   deleted (used to populate `stream_jobs.aggregated_text` on Finish/Error).

// ── Streaming message RAII guard ──
// Ensures streaming messages are finalized in DB even if the converter task
// panics or exits unexpectedly (e.g. engine panic, tokio cancellation).

pub(super) struct StreamingMessageGuard {
    db: sqlx::PgPool,
    msg_id: uuid::Uuid,
    session_id: Option<uuid::Uuid>,
    finalized: bool,
}

impl StreamingMessageGuard {
    pub(super) fn new(db: sqlx::PgPool, msg_id: uuid::Uuid) -> Self {
        Self { db, msg_id, session_id: None, finalized: false }
    }
    pub(super) fn set_session_id(&mut self, sid: uuid::Uuid) {
        self.session_id = Some(sid);
    }
    pub(super) fn mark_finalized(&mut self) {
        self.finalized = true;
    }
}

impl Drop for StreamingMessageGuard {
    fn drop(&mut self) {
        if !self.finalized
            && let Some(_sid) = self.session_id {
                let db = self.db.clone();
                let mid = self.msg_id;
                tokio::spawn(async move {
                    if let Err(e) = crate::db::sessions::finalize_streaming_message(&db, mid).await {
                        tracing::warn!(error = %e, msg_id = %mid, "failed to finalize streaming message in guard Drop");
                    }
                });
            }
    }
}

// ── SSE flush helpers (bounded text accumulation + delta tools) ──

/// Build tools JSON from accumulated tools, reusing cached value when no new tools arrived.
/// Only calls `.to_vec()` when `accumulated_tools` actually grew since the last build.
pub(super) fn build_tools_json(
    tools: &[serde_json::Value],
    flushed_count: &mut usize,
    cache: &mut Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    if tools.is_empty() {
        return None;
    }
    if cache.is_none() || tools.len() != *flushed_count {
        *cache = Some(serde_json::Value::Array(tools.to_vec()));
        *flushed_count = tools.len();
    }
    cache.clone()
}

/// Append-mode streaming message upsert. Text is APPENDED to existing content (not replaced).
/// Used for bounded text accumulation -- caller clears `accumulated_text` after success.
/// Also touches session activity for watchdog heartbeat, mirroring `upsert_streaming_message` behavior.
///
/// Invariant (Bug 2 fix, 2026-04-20): on INSERT we anchor `parent_message_id`
/// to the most-recent `role='user'` row for this session via a correlated
/// subquery. `bootstrap::run` persists the user row BEFORE the streaming row
/// is ever written, so the subquery is guaranteed to find a candidate.
/// `ON CONFLICT DO UPDATE` continues to append (`content || $3`) and refresh
/// `tool_calls`, but it deliberately does NOT touch `parent_message_id` —
/// the parent is pinned at first INSERT.
pub(super) async fn upsert_streaming_append(
    db: &sqlx::PgPool,
    message_id: uuid::Uuid,
    session_id: uuid::Uuid,
    agent_id: &str,
    text_delta: &str,
    tool_calls: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, tool_calls, agent_id, status, parent_message_id) \
         VALUES ( \
             $1, $2, 'assistant', $3, $4, $5, 'streaming', \
             (SELECT id FROM messages \
              WHERE session_id = $2 AND role = 'user' \
              ORDER BY created_at DESC \
              LIMIT 1) \
         ) \
         ON CONFLICT (id) DO UPDATE SET content = messages.content || $3, tool_calls = $4",
    )
    .bind(message_id)
    .bind(session_id)
    .bind(text_delta)
    .bind(tool_calls)
    .bind(agent_id)
    .execute(db)
    .await?;
    crate::db::sessions::touch_session_activity(db, session_id)
        .await
        .ok();
    Ok(())
}

/// Read the accumulated content from a streaming message row.
/// Used at Finish/Error/unexpected-exit to get full text for `stream_jobs` `set_content`,
/// since `accumulated_text` is cleared after each periodic flush.
pub(super) async fn read_streaming_content(db: &sqlx::PgPool, message_id: uuid::Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT COALESCE(content, '') FROM messages WHERE id = $1")
        .bind(message_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_tools_json_empty_returns_none() {
        let mut count = 0usize;
        let mut cache = None;
        assert!(build_tools_json(&[], &mut count, &mut cache).is_none());
    }

    #[test]
    fn build_tools_json_first_call_builds_array() {
        let tools = vec![serde_json::json!({"name": "search"})];
        let mut count = 0usize;
        let mut cache = None;
        let result = build_tools_json(&tools, &mut count, &mut cache).unwrap();
        assert_eq!(result, serde_json::json!([{"name": "search"}]));
        assert_eq!(count, 1);
    }

    #[test]
    fn build_tools_json_same_count_reuses_cache() {
        let tools = vec![serde_json::json!({"name": "search"})];
        let mut count = 0usize;
        let mut cache = None;
        build_tools_json(&tools, &mut count, &mut cache);
        let sentinel = serde_json::json!("SENTINEL");
        cache = Some(sentinel.clone());
        let result = build_tools_json(&tools, &mut count, &mut cache).unwrap();
        assert_eq!(result, sentinel);
    }

    #[test]
    fn build_tools_json_new_tool_invalidates_cache() {
        let tools_1 = vec![serde_json::json!({"name": "search"})];
        let mut count = 0usize;
        let mut cache = None;
        build_tools_json(&tools_1, &mut count, &mut cache);

        let tools_2 = vec![
            serde_json::json!({"name": "search"}),
            serde_json::json!({"name": "write"}),
        ];
        let result = build_tools_json(&tools_2, &mut count, &mut cache).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 2);
        assert_eq!(count, 2);
    }
}
