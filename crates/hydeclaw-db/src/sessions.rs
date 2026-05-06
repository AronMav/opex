use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Row};
use uuid::Uuid;

use crate::session_status::SessionStatus;

/// Maximum number of bind parameters per single SQL round-trip.
///
/// PostgreSQL's extended-query wire protocol uses a 16-bit length field for
/// the parameter count, giving a hard ceiling of 65535. We choose half that
/// (32767) as a conservative boundary to leave headroom for planner
/// overhead and for future column additions on tables where we batch.
///
/// CONTEXT.md correction #5: chunk_size = MAX_PARAMS_PER_QUERY / BIND_COUNT_PER_ROW,
/// where BIND_COUNT_PER_ROW counts ONLY the `$N` placeholders per VALUES row,
/// NOT the target-list column count. Literal SQL values (`'tool'`, `NOW()`,
/// `'complete'`) do NOT count toward the bind budget.
pub const MAX_PARAMS_PER_QUERY: usize = 32767;

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct Session {
    pub id: uuid::Uuid,
    pub agent_id: String,
    pub user_id: String,
    pub channel: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub last_message_at: chrono::DateTime<chrono::Utc>,
    pub title: Option<String>,
    #[cfg_attr(feature = "ts-gen", ts(type = "Record<string, unknown> | null"))]
    pub metadata: Option<serde_json::Value>,
    #[sqlx(default)]
    pub run_status: Option<String>,
    #[sqlx(default)]
    #[serde(skip)]
    pub activity_at: Option<chrono::DateTime<chrono::Utc>>,
    #[sqlx(default)]
    pub participants: Vec<String>,
    #[sqlx(default)]
    #[serde(skip)]
    pub retry_count: i32,
    #[sqlx(default)]
    pub parent_session_id: Option<uuid::Uuid>,
    #[sqlx(default)]
    pub end_reason: Option<String>,
}

/// Resolve `(user_id, channel)` lookup keys based on the agent's DM scope.
///
/// Pure function — used by `get_or_create_session`, `resolve_active_dm_session`,
/// and the channel WS dispatcher (`SessionKey`) so all three derive the same
/// logical session identifier.
///
/// - `"per-channel-peer"` (default): `(user_id, channel)` — distinct sessions
///   per chat platform for the same user.
/// - `"shared"` / `"per-peer"`: `(user_id, "*")` — single cross-channel DM.
/// - `"per-chat"`: `("*", channel)` — group-chat sessions keyed only on
///   channel.
/// - Any other value falls back to `per-channel-peer` (matches the legacy
///   wildcard arm in `get_or_create_session`).
pub fn dm_scope_keys<'a>(
    user_id: &'a str,
    channel: &'a str,
    dm_scope: &str,
) -> (&'a str, &'a str) {
    match dm_scope {
        "shared" | "per-peer" => (user_id, "*"),
        "per-chat" => ("*", channel),
        _ => (user_id, channel),
    }
}

/// Look up the most recent active DM session for `(agent, user, channel)`
/// after applying `dm_scope`, or `None` if none qualifies.
///
/// "Active" means `last_message_at > now() - 4h` AND `run_status` is one of
/// `{NULL, 'running', 'done'}`. Sessions in soft-terminal states
/// (`'failed'`, `'interrupted'`, `'timeout'`, `'cancelled'`) are excluded —
/// their context is potentially polluted, and silently picking up a
/// poisoned conversation surprises users.
///
/// Returns `(session_id, parsed_run_status)`. The status is parsed via
/// `SessionStatus::parse` so callers can hand it to `ReentryMode::classify`.
///
/// Read-only — no writes, no transactions, no advisory locks. Use
/// `get_or_create_session` when you need create-on-miss semantics.
pub async fn resolve_active_dm_session(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    channel: &str,
    dm_scope: &str,
) -> Result<Option<(Uuid, Option<SessionStatus>)>> {
    let (eff_user, eff_channel) = dm_scope_keys(user_id, channel, dm_scope);
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, run_status FROM sessions \
         WHERE agent_id = $1 AND user_id = $2 AND channel = $3 \
           AND last_message_at > now() - interval '4 hours' \
           AND (run_status IS NULL OR run_status IN ('running', 'done')) \
         ORDER BY last_message_at DESC LIMIT 1",
    )
    .bind(agent_id)
    .bind(eff_user)
    .bind(eff_channel)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|(id, status_str)| {
        let status = status_str.as_deref().and_then(SessionStatus::parse);
        (id, status)
    }))
}

/// Find or create a session for the user+agent pair, returning the resolved
/// session id together with a `ReentryMode` describing what kind of entry
/// this is (new row, continuation after `done`, resume of a still-`running`
/// session).
///
/// Sessions in soft-terminal statuses (`'failed'`, `'interrupted'`,
/// `'timeout'`, `'cancelled'`) are NOT reused — they are filtered by the
/// same WHERE clause `resolve_active_dm_session` uses, and a fresh row is
/// created instead. Rationale: previous run failed; the next user message
/// should not silently inherit a polluted context.
///
/// `dm_scope` controls session isolation (see `dm_scope_keys`).
pub async fn get_or_create_session(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    channel: &str,
    dm_scope: &str,
) -> Result<(Uuid, crate::ReentryMode)> {
    let (eff_user, eff_channel) = dm_scope_keys(user_id, channel, dm_scope);

    // Advisory lock keyed on (agent_id, user_id, channel) hash prevents concurrent
    // transactions from both inserting when no session exists. The CTE alone is NOT
    // sufficient — PostgreSQL snapshot isolation lets two concurrent CTEs both see
    // `existing` as empty and both INSERT. The advisory lock serializes access.
    let mut tx = db.begin().await?;

    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1 || $2 || $3))")
        .bind(agent_id)
        .bind(eff_user)
        .bind(eff_channel)
        .execute(&mut *tx)
        .await?;

    // Same filter as resolve_active_dm_session — soft-terminal statuses are
    // skipped so a fresh row is inserted on miss instead of resurrecting a
    // poisoned context. The `was_new` boolean disambiguates existing-vs-new
    // for ReentryMode classification.
    let row: (Uuid, Option<String>, bool) = sqlx::query_as(
        "WITH existing AS ( \
           SELECT id, run_status FROM sessions \
           WHERE agent_id = $1 AND user_id = $2 AND channel = $3 \
             AND last_message_at > now() - interval '4 hours' \
             AND (run_status IS NULL OR run_status IN ('running', 'done')) \
           ORDER BY last_message_at DESC LIMIT 1 \
         ), inserted AS ( \
           INSERT INTO sessions (agent_id, user_id, channel, participants) \
           SELECT $1, $2, $3, ARRAY[$1::text] \
           WHERE NOT EXISTS (SELECT 1 FROM existing) \
           RETURNING id \
         ) \
         SELECT id, run_status, false AS was_new FROM existing \
         UNION ALL \
         SELECT id, NULL::text AS run_status, true AS was_new FROM inserted \
         LIMIT 1",
    )
    .bind(agent_id)
    .bind(eff_user)
    .bind(eff_channel)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let (id, run_status_str, was_new) = row;
    let mode = if was_new {
        crate::ReentryMode::NewSession
    } else {
        let parsed = run_status_str.as_deref().and_then(SessionStatus::parse);
        crate::ReentryMode::classify(parsed)
    };
    Ok((id, mode))
}

/// Load session metadata needed for chain split operations.
pub async fn get_session_for_chain(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
) -> anyhow::Result<Option<(String, String, String, Option<String>)>> {
    let row = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT agent_id, user_id, channel, title FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Create a brand-new session for the given user (no history reuse).
/// Used by UI "New Chat" button to guarantee a fresh session.
pub async fn create_new_session(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    channel: &str,
) -> Result<Uuid> {
    let row = sqlx::query(
        "INSERT INTO sessions (agent_id, user_id, channel, participants) \
         VALUES ($1, $2, $3, ARRAY[$1]) RETURNING id",
    )
    .bind(agent_id)
    .bind(user_id)
    .bind(channel)
    .fetch_one(db)
    .await?;

    Ok(row.get("id"))
}

/// Create a child session in a compression chain.
pub async fn create_chain_session(
    db: &sqlx::PgPool,
    parent_id: uuid::Uuid,
    agent_id: &str,
    user_id: &str,
    channel: &str,
    title: Option<&str>,
) -> anyhow::Result<uuid::Uuid> {
    let id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, parent_session_id, agent_id, user_id, channel, title, participants)
         VALUES ($1, $2, $3, $4, $5, $6, ARRAY[$3])",
    )
    .bind(id)
    .bind(parent_id)
    .bind(agent_id)
    .bind(user_id)
    .bind(channel)
    .bind(title)
    .execute(db)
    .await?;
    Ok(id)
}

/// Mark a session as ended with a specific reason (e.g. "compression").
pub async fn set_session_end_reason(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    end_reason: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE sessions SET end_reason = $1 WHERE id = $2")
        .bind(end_reason)
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Insert compressed seed messages into a child session.
/// `messages` is ordered: [system?, summary(assistant), ...tail].
/// Each message gets a sequential `created_at` offset to preserve order.
pub async fn insert_seed_messages(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    agent_id: &str,
    messages: &[hydeclaw_types::Message],
) -> anyhow::Result<()> {
    use chrono::Utc;
    for (i, msg) in messages.iter().enumerate() {
        let role: &str = match msg.role {
            hydeclaw_types::MessageRole::System    => "system",
            hydeclaw_types::MessageRole::User      => "user",
            hydeclaw_types::MessageRole::Assistant => "assistant",
            hydeclaw_types::MessageRole::Tool      => "tool",
        };
        let tool_calls = msg.tool_calls.as_ref()
            .and_then(|tc| serde_json::to_value(tc).ok());
        sqlx::query(
            "INSERT INTO messages (id, session_id, agent_id, role, content, tool_calls, tool_call_id, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(session_id)
        .bind(agent_id)
        .bind(role)
        .bind(&msg.content)
        .bind(tool_calls)
        .bind(&msg.tool_call_id)
        .bind(Utc::now() + chrono::Duration::microseconds(i as i64))
        .execute(db)
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct SessionChainEntry {
    pub id: uuid::Uuid,
    pub parent_session_id: Option<uuid::Uuid>,
    pub end_reason: Option<String>,
    pub title: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub agent_id: String,
    pub depth: i64,
}

/// Return the full ancestor chain for `session_id`, ordered root-first.
/// `depth=0` = the queried session. Capped at 20 levels to prevent infinite loops.
pub async fn get_session_chain(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
) -> anyhow::Result<Vec<SessionChainEntry>> {
    let rows = sqlx::query_as::<_, SessionChainEntry>(
        "WITH RECURSIVE chain AS (
          SELECT id, parent_session_id, end_reason, title, started_at, agent_id,
                 0::bigint AS depth
          FROM sessions WHERE id = $1
          UNION ALL
          SELECT s.id, s.parent_session_id, s.end_reason, s.title, s.started_at, s.agent_id,
                 c.depth + 1
          FROM sessions s
          JOIN chain c ON s.id = c.parent_session_id
          WHERE c.depth < 19
        )
        SELECT id, parent_session_id, end_reason, title, started_at, agent_id, depth
        FROM chain ORDER BY depth DESC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Create a brand-new isolated session (no history reuse).
/// Used by cron dynamic jobs so each run starts with a clean context.
pub async fn create_isolated_session_with_user(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    channel: &str,
) -> Result<Uuid> {
    let row = sqlx::query(
        "INSERT INTO sessions (agent_id, user_id, channel, participants) \
         VALUES ($1, $2, $3, ARRAY[$1]) RETURNING id",
    )
    .bind(agent_id)
    .bind(user_id)
    .bind(channel)
    .fetch_one(db)
    .await?;

    Ok(row.get("id"))
}

/// Set session title from user's first message if not already titled.
/// Truncates to ~60 chars on a word boundary.
pub async fn auto_title_session(db: &PgPool, session_id: Uuid, user_text: &str) -> Result<()> {
    if user_text.trim().is_empty() {
        return Ok(());
    }

    // Only set title if it's currently NULL
    let row = sqlx::query("SELECT title FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_optional(db)
        .await?;
    match row {
        Some(r) if r.get::<Option<String>, _>("title").is_some() => return Ok(()),
        None => return Ok(()),
        _ => {}
    }

    // Truncate to ~60 chars on word boundary
    let trimmed = user_text.trim();
    let title = if trimmed.len() <= 60 {
        trimmed.to_string()
    } else {
        let mut end = 60;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        // Find last space before boundary
        if let Some(pos) = trimmed[..end].rfind(' ') {
            format!("{}…", &trimmed[..pos])
        } else {
            format!("{}…", &trimmed[..end])
        }
    };

    sqlx::query("UPDATE sessions SET title = $1 WHERE id = $2 AND title IS NULL")
        .bind(&title)
        .bind(session_id)
        .execute(db)
        .await?;

    Ok(())
}

/// Resume an existing session by ID (update `last_message_at`).
/// Returns the `session_id` if found, error if not.
pub async fn resume_session(db: &PgPool, session_id: Uuid) -> Result<Uuid> {
    let rows = sqlx::query("UPDATE sessions SET last_message_at = now() WHERE id = $1")
        .bind(session_id)
        .execute(db)
        .await?;

    if rows.rows_affected() == 0 {
        anyhow::bail!("session not found: {session_id}");
    }
    Ok(session_id)
}

/// Save a message to the session history.
pub async fn save_message(
    db: &PgPool,
    session_id: Uuid,
    role: &str,
    content: &str,
    tool_calls: Option<&serde_json::Value>,
    tool_call_id: Option<&str>,
) -> Result<Uuid> {
    save_message_ex(db, session_id, role, content, tool_calls, tool_call_id, None, None, None).await
}

/// Save a message with optional per-message `agent_id` (for multi-agent discuss sessions).
#[allow(clippy::too_many_arguments)]
pub async fn save_message_ex(
    db: &PgPool,
    session_id: Uuid,
    role: &str,
    content: &str,
    tool_calls: Option<&serde_json::Value>,
    tool_call_id: Option<&str>,
    agent_id: Option<&str>,
    thinking_blocks: Option<&serde_json::Value>,
    parent_id: Option<Uuid>,
) -> Result<Uuid> {
    let id = sqlx::query_scalar(
        "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, agent_id, thinking_blocks, parent_message_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(tool_calls)
    .bind(tool_call_id)
    .bind(agent_id)
    .bind(thinking_blocks)
    .bind(parent_id)
    .fetch_one(db)
    .await?;

    Ok(id)
}

/// Same as [`save_message_ex`] but the caller supplies the row `id` explicitly.
///
/// This exists for durability paths (e.g. tool-result persistence in
/// `pipeline::parallel`) where the persist work is detached via
/// `tokio::spawn` so it can survive parent-task cancellation. The caller
/// pre-generates the UUID synchronously so the chain (`parent_message_id`)
/// can be stitched without waiting for the detached insert to return.
///
/// Idempotent against retries: ON CONFLICT (id) DO NOTHING. The first
/// insert wins; duplicate calls with the same id are no-ops.
#[allow(clippy::too_many_arguments)]
pub async fn save_message_ex_with_id(
    db: &PgPool,
    id: Uuid,
    session_id: Uuid,
    role: &str,
    content: &str,
    tool_calls: Option<&serde_json::Value>,
    tool_call_id: Option<&str>,
    agent_id: Option<&str>,
    thinking_blocks: Option<&serde_json::Value>,
    parent_id: Option<Uuid>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, tool_calls, tool_call_id, agent_id, thinking_blocks, parent_message_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(tool_calls)
    .bind(tool_call_id)
    .bind(agent_id)
    .bind(thinking_blocks)
    .bind(parent_id)
    .execute(db)
    .await?;

    Ok(())
}

/// Set the step_id column for an existing message row. Used by the agent
/// pipeline immediately after `save_message_ex_with_id` inserts an
/// intermediate iteration row, so the row gets a queryable step number
/// without bloating the main insert signature with a parameter most
/// callers don't need.
///
/// No-op if the row doesn't exist (e.g. the insert lost a race).
pub async fn update_message_step_id(
    db: &PgPool,
    id: Uuid,
    step_id: i32,
) -> Result<()> {
    sqlx::query("UPDATE messages SET step_id = $1 WHERE id = $2")
        .bind(step_id)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}



/// Load messages for a session. If `limit` is `Some`, returns at most that many rows.
pub async fn load_messages(
    db: &PgPool,
    session_id: Uuid,
    limit: Option<i64>,
) -> Result<Vec<MessageRow>> {
    let rows = match limit {
        Some(lim) => {
            sqlx::query_as::<_, MessageRow>(
                "SELECT * FROM (\
                   SELECT id, role, content, tool_calls, tool_call_id, created_at, agent_id, feedback, edited_at, status, thinking_blocks, parent_message_id, branch_from_message_id, abort_reason, is_mirror \
                   FROM messages WHERE session_id = $1 AND compressed = FALSE \
                   ORDER BY created_at DESC LIMIT $2\
                 ) sub ORDER BY created_at ASC",
            )
            .bind(session_id)
            .bind(lim)
            .fetch_all(db)
            .await?
        }
        None => {
            sqlx::query_as::<_, MessageRow>(
                "SELECT id, role, content, tool_calls, tool_call_id, created_at, agent_id, feedback, edited_at, status, thinking_blocks, parent_message_id, branch_from_message_id, abort_reason, is_mirror \
                 FROM messages WHERE session_id = $1 AND compressed = FALSE \
                 ORDER BY created_at ASC",
            )
            .bind(session_id)
            .fetch_all(db)
            .await?
        }
    };

    Ok(rows)
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct MessageRow {
    pub id: uuid::Uuid,
    pub role: String,
    pub content: String,
    #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
    pub tool_calls: Option<serde_json::Value>,
    pub tool_call_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub agent_id: Option<String>,
    pub feedback: Option<i16>,
    pub edited_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: String,
    #[sqlx(default)]
    #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
    pub thinking_blocks: Option<serde_json::Value>,
    #[sqlx(default)]
    pub parent_message_id: Option<uuid::Uuid>,
    #[sqlx(default)]
    pub branch_from_message_id: Option<uuid::Uuid>,
    #[sqlx(default)]
    pub abort_reason: Option<String>,
    #[sqlx(default)]
    pub is_mirror: bool,
}

/// Insert or update a streaming assistant message (called every ~2s during LLM response).
///
/// Invariant (Bug 2 fix, 2026-04-20): on INSERT we anchor `parent_message_id`
/// to the most-recent `role='user'` row for this session via a correlated
/// subquery. `bootstrap::run` persists the user row BEFORE the streaming row
/// is ever written, so the subquery is guaranteed to find a candidate; if a
/// pathological race leaves no user row, `parent_message_id` will be NULL
/// which matches pre-fix behaviour (no regression).
///
/// `ON CONFLICT DO UPDATE` deliberately does NOT touch `parent_message_id` —
/// the parent is established once at first INSERT and is invariant for the
/// row's lifetime. A later user row racing in must NOT flip the parent
/// (tested by `tests/integration_streaming_parent_link.rs`).
pub async fn upsert_streaming_message(
    db: &PgPool,
    message_id: Uuid,
    session_id: Uuid,
    agent_id: &str,
    content: &str,
    tool_calls: Option<&serde_json::Value>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, tool_calls, agent_id, status, parent_message_id) \
         VALUES ( \
             $1, $2, 'assistant', $3, $4, $5, 'streaming', \
             (SELECT id FROM messages \
              WHERE session_id = $2 AND role = 'user' \
              ORDER BY created_at DESC \
              LIMIT 1) \
         ) \
         ON CONFLICT (id) DO UPDATE SET content = $3, tool_calls = $4"
    )
    .bind(message_id)
    .bind(session_id)
    .bind(content)
    .bind(tool_calls)
    .bind(agent_id)
    .execute(db)
    .await?;
    // Update session heartbeat — watchdog reads this to detect inactivity
    touch_session_activity(db, session_id).await.ok();
    Ok(())
}

/// Mark a streaming message as complete (called when LLM response finishes).
pub async fn finalize_streaming_message(db: &PgPool, message_id: Uuid) -> Result<()> {
    // Delete the streaming placeholder — the engine saves the authoritative final message
    sqlx::query("DELETE FROM messages WHERE id = $1 AND status = 'streaming'")
        .bind(message_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Read-only helper: fetch the current `run_status` of a session as a String.
/// Returns `Ok(None)` if the row doesn't exist OR `run_status IS NULL`.
/// Used by `DefaultContextBuilder::build` to classify `resume_session_id`
/// re-entries and by `claim_session_with_retry` to reclassify on a TOCTOU
/// race.
pub async fn get_session_run_status(db: &PgPool, session_id: Uuid) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT run_status FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|(s,)| s))
}

/// Set `run_status` for a session (finalize path: running → terminal).
///
/// Only allows transitions from `running` or `NULL` (new session). This blocks
/// all terminal→terminal jumps (e.g. failed→done, interrupted→failed) at the
/// SQL level. `claim_session_running` keeps its own looser guard because it
/// must allow soft-terminal → running re-entry.
pub async fn set_session_run_status(db: &PgPool, session_id: Uuid, status: &str) -> Result<()> {
    sqlx::query(
        "UPDATE sessions SET run_status = $1 \
         WHERE id = $2 \
           AND run_status IS DISTINCT FROM 'done'"
    )
        .bind(status)
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Log a warning if the requested transition violates the session FSM.
/// Does NOT abort — the SQL guard is the hard barrier. This is an early
/// diagnostic for test failures and log analysis.
pub fn warn_invalid_transition(from: Option<SessionStatus>, to: SessionStatus, session_id: Uuid) {
    if let Some(f) = from
        && !f.can_transition_to(to)
    {
        tracing::warn!(
            from = f.as_str(), to = to.as_str(), %session_id,
            "session FSM violation: invalid status transition"
        );
    }
}

/// Try to atomically claim a session as `'running'`. Returns `true` when the
/// session exists and was claimed (regardless of previous status), `false` when
/// the session was not found. Allows re-entry from any status including `'done'`
/// so that users can continue completed conversations. The guard-drop race is
/// safe: `mark_session_run_status_if_running` (used by `SessionLifecycleGuard`)
/// guards `WHERE run_status = 'running'`, so a completed-then-reclaimed session
/// cannot be accidentally set to `'failed'` by a stale guard.
pub async fn claim_session_running(db: &PgPool, session_id: Uuid) -> Result<bool> {
    let rows = sqlx::query(
        "UPDATE sessions SET run_status = 'running' WHERE id = $1"
    )
        .bind(session_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

/// Mark any `status='streaming'` messages in `session_id` as `'interrupted'`.
///
/// Called in bootstrap just after `claim_session_running` so that a streaming
/// message left by a previous crashed run does not pollute the context of the
/// new run. Returns the number of rows updated (0 if none were streaming).
pub async fn cleanup_session_streaming_messages(
    db: &PgPool,
    session_id: Uuid,
) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE messages SET status = 'interrupted',
                content = COALESCE(NULLIF(content, ''), '[interrupted]')
         WHERE session_id = $1 AND status = 'streaming'",
    )
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

/// Transition `run_status` from `'running'` to `new_status`. No-op if the
/// session is already in any terminal state (`'done'`, `'failed'`,
/// `'interrupted'`, `'timeout'`, `'cancelled'`).
///
/// Used on the cancel-grace path in the chat handler to mark sessions
/// `'interrupted'` when the user aborted a stream that then exceeded the
/// grace window — this fires BEFORE `engine_handle.abort()` drops the
/// `SessionLifecycleGuard`, which in turn uses this same helper so its
/// `'failed'` fallback cannot overwrite an earlier `'interrupted'`.
///
/// Returns the number of rows updated (0 if the session was already
/// terminal, 1 otherwise).
pub async fn mark_session_run_status_if_running(
    db: &PgPool,
    session_id: Uuid,
    new_status: &str,
) -> Result<u64> {
    let rows = sqlx::query(
        "UPDATE sessions SET run_status = $1 WHERE id = $2 AND run_status = 'running'"
    )
        .bind(new_status)
        .bind(session_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(rows)
}

/// Touch `activity_at` heartbeat — called from `upsert_streaming_message` every ~2s.
pub async fn touch_session_activity(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE sessions SET activity_at = NOW() WHERE id = $1")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Find sessions stuck in 'running' with no activity for > `inactivity_secs` seconds.
/// Returns Vec<(`session_id`, `agent_id`)>.
pub async fn find_stale_running_sessions(
    db: &PgPool,
    inactivity_secs: u64,
) -> Result<Vec<(Uuid, String)>> {
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, agent_id FROM sessions
         WHERE run_status = 'running'
           AND COALESCE(activity_at, last_message_at) < NOW() - ($1 || ' seconds')::INTERVAL"
    )
    .bind(inactivity_secs as i64)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Find sessions stuck in 'running' where assistant never responded.
///
/// Phase 63 DATA-02: rewrite from correlated-subquery-per-row to a single-pass
/// window function. The `latest_msg` CTE labels every message with its reverse-
/// chronological rank within the session; the outer query filters sessions
/// where `rn = 1` (the latest message) matches the "stuck" shape:
///   - run_status='running' AND role='user'  (assistant never responded)
///   - run_status='done'    AND role='assistant' AND empty content + empty tool_calls
///
/// Single scan of `messages` + single scan of `sessions` — no correlated subquery.
pub async fn find_stuck_sessions(
    db: &PgPool,
    stale_secs: i64,
    max_retries: i32,
) -> Result<Vec<(Uuid, String)>> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "WITH latest_msg AS ( \
             SELECT \
                 session_id, \
                 id, \
                 role, \
                 content, \
                 tool_calls, \
                 ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY created_at DESC) AS rn \
             FROM messages \
         ) \
         SELECT s.id, s.agent_id FROM sessions s \
         LEFT JOIN latest_msg lm ON lm.session_id = s.id AND lm.rn = 1 \
         WHERE s.retry_count < $2 \
           AND COALESCE(s.activity_at, s.last_message_at) < NOW() - make_interval(secs => $1) \
           AND ( \
             (s.run_status = 'running' AND lm.role = 'user') \
             OR \
             (s.run_status = 'done' \
              AND lm.role = 'assistant' \
              AND (lm.content IS NULL OR lm.content = '') \
              AND (lm.tool_calls IS NULL OR lm.tool_calls = '[]'::jsonb)) \
           )"
    )
    .bind(stale_secs as f64)
    .bind(max_retries)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Increment retry_count atomically and set run_status to 'running'.
/// Returns None if concurrent retry already changed the status (prevents double-fire).
pub async fn increment_retry_count(db: &PgPool, session_id: Uuid) -> Result<Option<i32>> {
    let row: Option<(i32,)> = sqlx::query_as(
        "UPDATE sessions SET retry_count = retry_count + 1, run_status = 'running' \
         WHERE id = $1 AND run_status IN ('running', 'done') \
         RETURNING retry_count"
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(c,)| c))
}

/// Mark a session as permanently failed after max retries exhausted.
pub async fn mark_session_failed(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE sessions SET run_status = 'failed' WHERE id = $1")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Get the last user message text from a session (for retry).
pub async fn get_last_user_message(db: &PgPool, session_id: Uuid) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT content FROM messages \
         WHERE session_id = $1 AND role = 'user' \
         ORDER BY created_at DESC LIMIT 1"
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(c,)| c))
}

/// Delete empty assistant messages from a session (cleanup before retry).
pub async fn delete_empty_assistant_messages(db: &PgPool, session_id: Uuid) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM messages WHERE session_id = $1 AND role = 'assistant' \
         AND (content IS NULL OR content = '') \
         AND (tool_calls IS NULL OR tool_calls = '[]'::jsonb)"
    )
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Insert synthetic tool results for all unmatched tool calls in a session.
/// Called during startup cleanup and transcript repair.
/// Returns the number of synthetic results inserted.
pub async fn insert_synthetic_tool_results(db: &PgPool, session_id: Uuid) -> Result<usize> {
    // Find assistant messages with tool_calls that have no matching tool result
    let assistant_msgs = sqlx::query_as::<_, (Uuid, serde_json::Value)>(
        "SELECT id, tool_calls FROM messages
         WHERE session_id = $1 AND role = 'assistant'
           AND tool_calls IS NOT NULL AND jsonb_array_length(tool_calls) > 0
         ORDER BY created_at"
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;

    // Collect all tool_call_ids from assistant messages
    let mut all_call_ids: Vec<String> = Vec::new();
    for (_msg_id, tool_calls_json) in &assistant_msgs {
        let calls = match tool_calls_json.as_array() {
            Some(a) => a,
            None => continue,
        };
        for call in calls {
            if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                all_call_ids.push(id.to_string());
            }
        }
    }

    if all_call_ids.is_empty() {
        return Ok(0);
    }

    // Batch query: find which tool_call_ids already have a tool result
    let existing: Vec<String> = sqlx::query_scalar(
        "SELECT tool_call_id FROM messages WHERE session_id = $1 AND role = 'tool' AND tool_call_id = ANY($2)"
    )
    .bind(session_id)
    .bind(&all_call_ids)
    .fetch_all(db)
    .await?;

    let existing_set: std::collections::HashSet<&str> = existing.iter().map(std::string::String::as_str).collect();

    // Find missing tool_call_ids
    let missing: Vec<&str> = all_call_ids.iter()
        .map(std::string::String::as_str)
        .filter(|id| !existing_set.contains(id))
        .collect();

    if missing.is_empty() {
        return Ok(0);
    }

    // Phase 63 DATA-03: chunked batch INSERT.
    //
    // Row shape: (id, session_id, role, content, tool_call_id, created_at, status)
    //   - bound placeholders per row: id=$X, session_id=$Y, content=$Z, tool_call_id=$W  → 4 binds
    //   - literal SQL per row:        'tool', NOW(), 'complete'                          → 3 literals
    //
    // chunk_size = MAX_PARAMS_PER_QUERY / BIND_COUNT_PER_ROW = 32767 / 4 = 8191 rows.
    const BIND_COUNT_PER_ROW: usize = 4;
    let chunk_size: usize = MAX_PARAMS_PER_QUERY / BIND_COUNT_PER_ROW;

    let mut tx = db.begin().await?;
    let mut inserted: usize = 0;
    for chunk in missing.chunks(chunk_size) {
        let mut sql = String::from(
            "INSERT INTO messages (id, session_id, role, content, tool_call_id, created_at, status) VALUES ",
        );
        let mut placeholders: Vec<String> = Vec::with_capacity(chunk.len());
        for i in 0..chunk.len() {
            let base = i * BIND_COUNT_PER_ROW;
            placeholders.push(format!(
                "(${}, ${}, 'tool', ${}, ${}, NOW(), 'complete')",
                base + 1,
                base + 2,
                base + 3,
                base + 4
            ));
        }
        sql.push_str(&placeholders.join(", "));

        let mut q = sqlx::query(&sql);
        for call_id in chunk {
            let synthetic_id = uuid::Uuid::new_v4();
            q = q
                .bind(synthetic_id)
                .bind(session_id)
                .bind("[interrupted] Tool execution was interrupted (process restart). Result unavailable.")
                .bind(*call_id);
        }

        let result = q.execute(&mut *tx).await?;
        inserted += result.rows_affected() as usize;
    }
    tx.commit().await?;
    Ok(inserted)
}

/// Insert synthetic "[interrupted]" tool results for specific `tool_call_ids`.
/// Unlike `insert_synthetic_tool_results` (which scans the whole session),
/// this takes pre-filtered `call_ids` from the caller -- used by `build_context`
/// where the caller already knows which IDs are missing (ENG-01).
pub async fn insert_missing_tool_results(
    db: &PgPool,
    session_id: Uuid,
    call_ids: &[String],
) -> Result<()> {
    for call_id in call_ids {
        let synthetic_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, tool_call_id, created_at, status)
             VALUES ($1, $2, 'tool', $3, $4, NOW(), 'complete')"
        )
        .bind(synthetic_id)
        .bind(session_id)
        .bind("[interrupted] Tool execution was interrupted (process restart). Result unavailable.")
        .bind(call_id)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// Startup cleanup: find all sessions interrupted by crash, repair their transcripts,
/// delete orphaned streaming messages, mark as 'interrupted'.
/// Also handles old sessions with orphaned streaming messages (no `run_status` set).
/// Returns count so caller can loop in batches.
pub async fn cleanup_interrupted_sessions(db: &PgPool) -> Result<usize> {
    // Find sessions that were 'running' when the process died (batched)
    let interrupted = sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT s.id FROM sessions s
         WHERE s.run_status = 'running'
            OR EXISTS (
                SELECT 1 FROM messages m
                WHERE m.session_id = s.id AND m.status = 'streaming'
            )
         LIMIT 100"
    )
    .fetch_all(db)
    .await?;

    let count = interrupted.len();
    if count > 0 {
        tracing::info!(count, "cleaning up interrupted sessions");
    }

    for session_id in &interrupted {
        // 1. Insert synthetic tool results for incomplete tool calls
        if let Err(e) = insert_synthetic_tool_results(db, *session_id).await {
            tracing::warn!(error = %e, session_id = %session_id, "failed to insert synthetic tool results");
        }

        // 2. Mark orphaned streaming messages as interrupted (instead of deleting)
        if let Err(e) = sqlx::query(
            "UPDATE messages SET status = 'interrupted', content = COALESCE(NULLIF(content, ''), '[interrupted]')
             WHERE session_id = $1 AND status = 'streaming'"
        )
            .bind(session_id)
            .execute(db)
            .await
        {
            tracing::warn!(error = %e, session_id = %session_id, "failed to mark orphaned streaming messages");
        }

        // 3. Mark session as interrupted
        if let Err(e) = set_session_run_status(db, *session_id, "interrupted").await {
            tracing::warn!(error = %e, session_id = %session_id, "failed to mark session interrupted");
        }
    }

    // 4. Final safety check: any session still 'running' with no activity for 30m is 'interrupted'
    sqlx::query(
        "UPDATE sessions SET run_status = 'interrupted' \
         WHERE run_status = 'running' \
           AND COALESCE(activity_at, last_message_at) < NOW() - interval '30 minutes'"
    )
    .execute(db)
    .await?;

    // 5. Clear stale streamStatus from UI metadata.
    //    After a restart, no streams are active, so any session showing "streaming"
    //    in its UI metadata must be stale. Clear them all at once.
    if let Err(e) = sqlx::query(
        "UPDATE sessions
         SET metadata = jsonb_set(metadata, '{ui_state,streamStatus}', '\"idle\"')
         WHERE metadata->'ui_state'->>'streamStatus' = 'streaming'"
    )
    .execute(db)
    .await
    {
        tracing::warn!(error = %e, "failed to clear stale streamStatus from UI metadata");
    }

    Ok(count)
}

/// Delete sessions older than `ttl_days` and their messages (cascade).
pub async fn cleanup_old_sessions(db: &PgPool, ttl_days: u32) -> Result<u64> {
    if ttl_days == 0 {
        return Ok(0);
    }
    let result = sqlx::query(
        "DELETE FROM sessions WHERE last_message_at < now() - make_interval(days => $1)",
    )
    .bind(ttl_days as i32)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Delete sessions beyond `max_per_agent` for every agent, keeping the
/// most recent by `last_message_at`. Running sessions are preserved and
/// not counted toward the cap — they may be actively streaming.
/// A cap of 0 disables this cleanup.
pub async fn cleanup_excess_sessions_per_agent(
    db: &PgPool,
    max_per_agent: u32,
) -> Result<u64> {
    if max_per_agent == 0 {
        return Ok(0);
    }
    let result = sqlx::query(
        "WITH ranked AS ( \
           SELECT id, ROW_NUMBER() OVER ( \
             PARTITION BY agent_id ORDER BY last_message_at DESC \
           ) AS rn \
           FROM sessions \
           WHERE run_status IS NULL OR run_status != 'running' \
         ) \
         DELETE FROM sessions \
         WHERE id IN (SELECT id FROM ranked WHERE rn > $1)",
    )
    .bind(max_per_agent as i32)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Find the active session for a user+agent+channel pair (last 4 hours).
pub async fn find_active_session(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    channel: &str,
    dm_scope: &str,
) -> Result<Option<Uuid>> {
    let (eff_user, eff_channel) = match dm_scope {
        "shared" | "per-peer" => (user_id, "*"),
        "per-chat" => ("*", channel),
        _ => (user_id, channel),
    };

    let row = sqlx::query(
        "SELECT id FROM sessions \
         WHERE agent_id = $1 AND user_id = $2 AND channel = $3 \
           AND last_message_at > now() - interval '4 hours' \
         ORDER BY last_message_at DESC LIMIT 1",
    )
    .bind(agent_id)
    .bind(eff_user)
    .bind(eff_channel)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|r| r.get("id")))
}

/// Delete a specific session and its messages (cascade).
pub async fn delete_session(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Count messages in a session.
pub async fn count_messages(db: &PgPool, session_id: Uuid) -> Result<i64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE session_id = $1")
        .bind(session_id)
        .fetch_one(db)
        .await?;
    Ok(count)
}

/// Search messages across all agent sessions using `PostgreSQL` FTS.
/// Falls back to ILIKE if FTS column is not yet available.
pub async fn search_messages(
    db: &PgPool,
    agent_id: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<SearchResult>> {
    // Try FTS first (migration 017 adds tsv column)
    let rows = sqlx::query_as::<_, SearchResult>(
        "SELECT m.content, s.id as session_id, s.user_id, s.channel, m.role, m.created_at, \
                ts_rank_cd(m.tsv, plainto_tsquery('simple', $2))::float8 AS rank \
         FROM messages m JOIN sessions s ON m.session_id = s.id \
         WHERE s.agent_id = $1 AND m.tsv @@ plainto_tsquery('simple', $2) \
         ORDER BY rank DESC, m.created_at DESC LIMIT $3",
    )
    .bind(agent_id)
    .bind(query)
    .bind(limit)
    .fetch_all(db)
    .await;

    if let Ok(r) = rows { Ok(r) } else {
        // Fallback to ILIKE if tsv column doesn't exist yet.
        // Escape LIKE wildcards (%, _, \) to prevent wildcard injection.
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let rows = sqlx::query_as::<_, SearchResult>(
            "SELECT m.content, s.id as session_id, s.user_id, s.channel, m.role, m.created_at, \
                    0.0::float8 AS rank \
             FROM messages m JOIN sessions s ON m.session_id = s.id \
             WHERE s.agent_id = $1 AND m.content ILIKE '%' || $2 || '%' ESCAPE '\\' \
             ORDER BY m.created_at DESC LIMIT $3",
        )
        .bind(agent_id)
        .bind(&escaped)
        .bind(limit)
        .fetch_all(db)
        .await?;
        Ok(rows)
    }
}

#[derive(Debug, FromRow)]
pub struct SearchResult {
    pub content: String,
    pub session_id: Uuid,
    pub user_id: String,
    pub channel: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub rank: f64,
}

/// Get session metadata by ID.
pub async fn get_session(db: &PgPool, session_id: Uuid) -> Result<Option<Session>> {
    let row = sqlx::query_as::<_, Session>(
        "SELECT id, agent_id, user_id, channel, started_at, last_message_at, title, metadata, run_status, activity_at, participants, retry_count, parent_session_id, end_reason \
         FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;

    Ok(row)
}

/// Trim messages in a session, keeping only the most recent `max_messages`.
pub async fn trim_session_messages(db: &PgPool, session_id: Uuid, max_messages: u32) -> Result<u64> {
    if max_messages == 0 {
        return Ok(0);
    }
    let result = sqlx::query(
        "DELETE FROM messages WHERE session_id = $1 AND id NOT IN \
         (SELECT id FROM messages WHERE session_id = $1 ORDER BY created_at DESC LIMIT $2)",
    )
    .bind(session_id)
    .bind(i64::from(max_messages))
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Export a full session as JSON (metadata + all messages).
pub async fn export_session(db: &PgPool, session_id: Uuid) -> sqlx::Result<Option<serde_json::Value>> {
    // 1. Fetch session metadata
    let session = sqlx::query_as::<_, Session>(
        "SELECT id, agent_id, user_id, channel, started_at, last_message_at, title, metadata, run_status, activity_at, participants, retry_count, parent_session_id, end_reason \
         FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;

    let session = match session {
        Some(s) => s,
        None => return Ok(None),
    };

    // 2. Fetch all messages ordered by created_at ASC
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT id, role, content, tool_calls, tool_call_id, created_at, agent_id, feedback, edited_at, status, thinking_blocks, parent_message_id, branch_from_message_id, abort_reason, is_mirror \
         FROM messages WHERE session_id = $1 \
         ORDER BY created_at ASC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;

    let msg_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id.to_string(),
                "role": m.role,
                "content": m.content,
                "tool_calls": m.tool_calls,
                "tool_call_id": m.tool_call_id,
                "created_at": m.created_at.to_rfc3339(),
                "agent_id": m.agent_id,
                "feedback": m.feedback.unwrap_or(0),
                "edited_at": m.edited_at.map(|t| t.to_rfc3339()),
                "status": m.status,
                "is_mirror": m.is_mirror,
            })
        })
        .collect();

    // 3. Return as JSON with version field
    Ok(Some(serde_json::json!({
        "version": 1,
        "session": {
            "id": session.id.to_string(),
            "agent_id": session.agent_id,
            "user_id": session.user_id,
            "channel": session.channel,
            "started_at": session.started_at.to_rfc3339(),
            "last_message_at": session.last_message_at.to_rfc3339(),
            "title": session.title,
            "metadata": session.metadata,
            "run_status": session.run_status,
            "participants": session.participants,
        },
        "messages": msg_json,
        "message_count": msg_json.len(),
    })))
}

/// Add an agent to a session's participants list (idempotent).
pub async fn add_participant(db: &PgPool, session_id: Uuid, agent_name: &str) -> Result<Vec<String>> {
    let row = sqlx::query(
        "UPDATE sessions SET participants = array_append(participants, $2) \
         WHERE id = $1 AND NOT ($2 = ANY(participants)) \
         RETURNING participants"
    )
    .bind(session_id)
    .bind(agent_name)
    .fetch_optional(db)
    .await?;
    if let Some(r) = row { Ok(r.get("participants")) } else {
        // Agent was already a participant — return current list
        let r = sqlx::query("SELECT participants FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(db)
            .await?;
        Ok(r.get("participants"))
    }
}

/// Get participants for a session.
pub async fn get_participants(db: &PgPool, session_id: Uuid) -> Result<Vec<String>> {
    let row = sqlx::query("SELECT participants FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_one(db)
        .await?;
    Ok(row.get("participants"))
}

/// Get the most recent UI session for an agent (within 4-hour window).
pub async fn get_latest_ui_session(db: &PgPool, agent_id: &str) -> Result<Option<Session>> {
    let session = sqlx::query_as::<_, Session>(
        "SELECT id, agent_id, user_id, channel, started_at, last_message_at, title, metadata, run_status, activity_at, participants, retry_count, parent_session_id, end_reason \
         FROM sessions \
         WHERE agent_id = $1 AND channel = 'ui' \
           AND last_message_at > now() - interval '4 hours' \
         ORDER BY last_message_at DESC \
         LIMIT 1",
    )
    .bind(agent_id)
    .fetch_optional(db)
    .await?;

    Ok(session)
}

// ── Branching support ────────────────────────────────────────────────────────

/// Walk the `parent_message_id` chain from `leaf_message_id` back to root,
/// returning messages in chronological (root-first) order.
pub async fn load_branch_messages(
    db: &PgPool,
    session_id: Uuid,
    leaf_message_id: Uuid,
) -> Result<Vec<MessageRow>> {
    // Use a recursive CTE to walk the parent chain from leaf to root
    let rows = sqlx::query_as::<_, MessageRow>(
        "WITH RECURSIVE chain AS (\
           SELECT id, role, content, tool_calls, tool_call_id, created_at, agent_id, feedback, edited_at, status, thinking_blocks, parent_message_id, branch_from_message_id, abort_reason, is_mirror, compressed \
           FROM messages WHERE id = $1 AND session_id = $2 \
           UNION ALL \
           SELECT m.id, m.role, m.content, m.tool_calls, m.tool_call_id, m.created_at, m.agent_id, m.feedback, m.edited_at, m.status, m.thinking_blocks, m.parent_message_id, m.branch_from_message_id, m.abort_reason, m.is_mirror, m.compressed \
           FROM messages m INNER JOIN chain c ON m.id = c.parent_message_id WHERE m.session_id = $2\
         ) SELECT id, role, content, tool_calls, tool_call_id, created_at, agent_id, feedback, edited_at, status, thinking_blocks, parent_message_id, branch_from_message_id, abort_reason, is_mirror \
           FROM chain WHERE compressed = FALSE ORDER BY created_at ASC",
    )
    .bind(leaf_message_id)
    .bind(session_id)
    .fetch_all(db)
    .await?;

    Ok(rows)
}

/// Resolve the active path for a session.
/// If `leaf_message_id` is provided, returns the branch chain ending at that message.
/// If `None`, finds the latest leaf (a message with no children) and returns its chain.
/// Falls back to flat history when no branching columns are populated.
pub async fn resolve_active_path(
    db: &PgPool,
    session_id: Uuid,
    leaf_message_id: Option<Uuid>,
) -> Result<Vec<MessageRow>> {
    if let Some(leaf_id) = leaf_message_id {
        return load_branch_messages(db, session_id, leaf_id).await;
    }

    // Auto-detect latest leaf: find messages that are not a parent of any other message
    let leaf_row = sqlx::query(
        "SELECT m.id FROM messages m \
         WHERE m.session_id = $1 \
           AND NOT EXISTS (SELECT 1 FROM messages c WHERE c.parent_message_id = m.id AND c.session_id = $1) \
         ORDER BY m.created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;

    match leaf_row {
        Some(row) => {
            let leaf_id: Uuid = row.get("id");
            load_branch_messages(db, session_id, leaf_id).await
        }
        // No branching data — fall back to flat history
        None => load_messages(db, session_id, None).await,
    }
}

/// Find the parent of a given message (the message immediately before it in chronological order).
/// Returns `None` if the message is the first in the session.
pub async fn find_parent_of_message(
    db: &PgPool,
    session_id: Uuid,
    message_id: Uuid,
) -> Result<Option<Uuid>> {
    let row: Option<(Option<Uuid>,)> = sqlx::query_as(
        "SELECT parent_message_id FROM messages WHERE id = $1 AND session_id = $2",
    )
    .bind(message_id)
    .bind(session_id)
    .fetch_optional(db)
    .await?;

    if let Some((parent_id,)) = row { Ok(parent_id) } else {
        // Message not found — fall back to chronological ordering
        let prev: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM messages WHERE session_id = $1 AND created_at < \
             (SELECT created_at FROM messages WHERE id = $2) \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(session_id)
        .bind(message_id)
        .fetch_optional(db)
        .await?;
        Ok(prev.map(|(id,)| id))
    }
}

/// Fork a session: insert a new message with parent and branch-from references.
#[allow(clippy::too_many_arguments)]
pub async fn save_message_branched(
    db: &PgPool,
    session_id: Uuid,
    role: &str,
    content: &str,
    tool_calls: Option<&serde_json::Value>,
    tool_call_id: Option<&str>,
    agent_id: Option<&str>,
    thinking_blocks: Option<&serde_json::Value>,
    parent_message_id: Option<Uuid>,
    branch_from_message_id: Option<Uuid>,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, tool_calls, tool_call_id, agent_id, thinking_blocks, parent_message_id, branch_from_message_id, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'complete')",
    )
    .bind(id)
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(tool_calls)
    .bind(tool_call_id)
    .bind(agent_id)
    .bind(thinking_blocks)
    .bind(parent_message_id)
    .bind(branch_from_message_id)
    .execute(db)
    .await?;

    Ok(id)
}

/// Maximum bytes persisted for a partial assistant message produced before
/// a cancel-class `LlmCallError` surfaced. Spec §5.
pub const MAX_PARTIAL_BYTES: usize = 256 * 1024;

/// Truncate `content` to at most `MAX_PARTIAL_BYTES`, respecting UTF-8
/// char boundaries (never returns a slice that splits a codepoint).
///
/// Exposed as a pure helper so the truncation invariant can be unit-tested
/// without a live database.
pub fn truncate_partial(content: &str) -> &str {
    if content.len() <= MAX_PARTIAL_BYTES {
        return content;
    }
    // Walk back from the cap to the nearest char boundary so we never
    // return a slice that splits a multi-byte UTF-8 codepoint.
    let mut end = MAX_PARTIAL_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

/// Persist a partial assistant message produced before a cancel-class
/// `LlmCallError` surfaced. `content` is truncated to `MAX_PARTIAL_BYTES`.
///
/// `abort_reason` should come from `LlmCallError::abort_reason()` — stable
/// short identifiers: `connect_timeout` | `inactivity` | `request_timeout`
/// | `max_duration` | `user_cancelled` | `shutdown_drain`. Changing these
/// strings breaks historical rows (see migration 024).
///
/// Inserts a row with `role = 'assistant'`, `status = 'aborted'`. The caller
/// is responsible for deciding whether to call this — only variants whose
/// `partial_text()` returns `Some` and is non-empty should be persisted.
pub async fn insert_assistant_partial(
    db: &PgPool,
    session_id: Uuid,
    agent_id: Option<&str>,
    content: &str,
    abort_reason: Option<&str>,
    parent_message_id: Option<Uuid>,
) -> Result<Uuid> {
    let truncated = truncate_partial(content);
    let id = Uuid::new_v4();
    // `status = 'aborted'` is the same stable string as `db::usage::STATUS_ABORTED`
    // (see migration 025). Not referenced here as a Rust constant because the
    // `db` lib-facade (src/lib.rs) re-exports `db::sessions` as a leaf module
    // without `db::usage`; pulling usage into sessions would cascade the lib
    // surface. The schema pins the literal on the DB side.
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, agent_id, status, abort_reason, parent_message_id) \
         VALUES ($1, $2, 'assistant', $3, $4, 'aborted', $5, $6)",
    )
    .bind(id)
    .bind(session_id)
    .bind(truncated)
    .bind(agent_id)
    .bind(abort_reason)
    .bind(parent_message_id)
    .execute(db)
    .await?;
    Ok(id)
}

/// Append a delivery-mirror record to the most recent DM session for the given
/// agent + channel + participant.
///
/// Returns `Ok(true)` if a matching session was found and the record inserted.
/// Returns `Ok(false)` if no matching DM session exists (group chat, no prior conversation).
/// Never fails fatally — callers should fire-and-forget via `tokio::spawn`.
pub async fn mirror_to_session(
    db: &PgPool,
    agent_id: &str,
    channel: &str,
    participant_id: &str,
    text: &str,
) -> anyhow::Result<bool> {
    // Find the most recent DM session for this agent+channel+participant.
    // Excludes per-chat group sessions (user_id = '*').
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM sessions \
         WHERE agent_id = $1 \
           AND channel   = $2 \
           AND user_id   = $3 \
           AND user_id  != '*' \
         ORDER BY started_at DESC \
         LIMIT 1",
    )
    .bind(agent_id)
    .bind(channel)
    .bind(participant_id)
    .fetch_optional(db)
    .await?;

    let session_id = match row {
        Some((id,)) => id,
        None => return Ok(false),
    };

    sqlx::query(
        "INSERT INTO messages (session_id, agent_id, role, content, is_mirror) \
         VALUES ($1, $2, 'assistant', $3, true)",
    )
    .bind(session_id)
    .bind(agent_id)
    .bind(text)
    .execute(db)
    .await?;

    Ok(true)
}

// ── Compression tracking ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompressionEventRow {
    pub segment_index: i64,
    pub first_live_message_id: Option<uuid::Uuid>,
    pub summary: String,
}

#[derive(Debug)]
pub struct MessagesPage {
    pub messages: Vec<MessageRow>,
    pub compression_events: Vec<CompressionEventRow>,
    pub has_more: bool,
}

/// Mark a batch of messages as compressed (excluded from LLM context).
pub async fn mark_messages_compressed(
    db: &PgPool,
    ids: &[uuid::Uuid],
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE messages SET compressed = TRUE WHERE id = ANY($1)",
    )
    .bind(ids)
    .execute(db)
    .await?;
    Ok(())
}

/// Insert a session_events WAL record for a compression boundary.
#[allow(clippy::too_many_arguments)]
pub async fn insert_compression_event(
    db: &PgPool,
    session_id: uuid::Uuid,
    segment_index: u32,
    summary: &str,
    first_compressed_id: Option<uuid::Uuid>,
    first_live_id: Option<uuid::Uuid>,
    tokens_before: i64,
    tokens_after: i64,
) -> Result<()> {
    let payload = serde_json::json!({
        "segment_index": segment_index,
        "summary": summary,
        "first_compressed_message_id": first_compressed_id,
        "first_live_message_id": first_live_id,
        "tokens_before": tokens_before,
        "tokens_after": tokens_after,
    });
    sqlx::query(
        "INSERT INTO session_events (session_id, event_type, payload)
         VALUES ($1, 'compression', $2)",
    )
    .bind(session_id)
    .bind(payload)
    .execute(db)
    .await?;
    Ok(())
}

/// Load a page of non-compressed messages with optional backward cursor.
///
/// Returns messages in ASC order (oldest first). Compression events whose
/// `first_live_message_id` falls within the returned page are included so
/// the frontend can render dividers.
pub async fn get_messages_page(
    db: &PgPool,
    session_id: uuid::Uuid,
    before_id: Option<uuid::Uuid>,
    limit: i64,
) -> Result<MessagesPage> {
    // Fetch limit+1 in DESC order to detect has_more, then reverse to ASC.
    let rows: Vec<MessageRow> = if let Some(bid) = before_id {
        sqlx::query_as::<_, MessageRow>(
            r#"SELECT id, role, content, tool_calls, tool_call_id, created_at,
                      agent_id, feedback, edited_at, status, thinking_blocks,
                      parent_message_id, branch_from_message_id, abort_reason, is_mirror
               FROM messages
               WHERE session_id = $1
                 AND compressed = FALSE
                 AND created_at < (SELECT created_at FROM messages WHERE id = $2)
               ORDER BY created_at DESC
               LIMIT $3"#,
        )
        .bind(session_id)
        .bind(bid)
        .bind(limit + 1)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query_as::<_, MessageRow>(
            r#"SELECT id, role, content, tool_calls, tool_call_id, created_at,
                      agent_id, feedback, edited_at, status, thinking_blocks,
                      parent_message_id, branch_from_message_id, abort_reason, is_mirror
               FROM messages
               WHERE session_id = $1
                 AND compressed = FALSE
               ORDER BY created_at DESC
               LIMIT $2"#,
        )
        .bind(session_id)
        .bind(limit + 1)
        .fetch_all(db)
        .await?
    };

    let has_more = rows.len() as i64 > limit;
    let mut rows: Vec<MessageRow> = rows.into_iter().take(limit as usize).collect();
    rows.reverse(); // ASC: oldest first

    // Fetch compression events whose first_live_message_id is in this page.
    let page_ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
    let events = if page_ids.is_empty() {
        vec![]
    } else {
        sqlx::query(
            r#"SELECT payload
               FROM session_events
               WHERE session_id = $1
                 AND event_type = 'compression'
                 AND (payload->>'first_live_message_id')::uuid = ANY($2)"#,
        )
        .bind(session_id)
        .bind(&page_ids[..])
        .fetch_all(db)
        .await?
        .into_iter()
        .filter_map(|r| {
            let p: Option<serde_json::Value> = r.try_get("payload").ok()?;
            let p = p?;
            Some(CompressionEventRow {
                segment_index: p["segment_index"].as_i64().unwrap_or(0),
                first_live_message_id: p["first_live_message_id"]
                    .as_str()
                    .and_then(|s| s.parse().ok()),
                summary: p["summary"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect()
    };

    Ok(MessagesPage { messages: rows, compression_events: events, has_more })
}

#[cfg(test)]
mod tests {
    use super::{truncate_partial, MAX_PARTIAL_BYTES};

    #[test]
    fn message_row_has_thinking_blocks_field() {
        let _ = |row: super::MessageRow| {
            let _: Option<serde_json::Value> = row.thinking_blocks;
        };
    }

    #[test]
    fn message_row_has_branching_fields() {
        let _ = |row: super::MessageRow| {
            let _: Option<uuid::Uuid> = row.parent_message_id;
            let _: Option<uuid::Uuid> = row.branch_from_message_id;
        };
    }

    #[test]
    fn truncate_partial_caps_at_256_kib() {
        let content = "a".repeat(MAX_PARTIAL_BYTES + 100);
        let out = truncate_partial(&content);
        assert_eq!(out.len(), MAX_PARTIAL_BYTES);
    }

    #[test]
    fn truncate_partial_passes_through_small_content() {
        let content = "hello world";
        let out = truncate_partial(content);
        assert_eq!(out, content);
    }

    #[test]
    fn truncate_partial_respects_char_boundary() {
        // 4-byte codepoint repeated enough times that the raw cap falls
        // inside a multi-byte sequence; truncation must walk back.
        let glyph = "😀"; // 4 bytes
        // Build a string whose length > MAX_PARTIAL_BYTES and where
        // MAX_PARTIAL_BYTES is NOT a char boundary: 262_144 / 4 = 65_536
        // (exact multiple — boundary lands cleanly), so we prepend one
        // ASCII byte to force the boundary off.
        let mut content = String::with_capacity(MAX_PARTIAL_BYTES + 8);
        content.push('x'); // 1-byte prefix
        while content.len() < MAX_PARTIAL_BYTES + 4 {
            content.push_str(glyph);
        }
        let out = truncate_partial(&content);
        // out.len() must be <= MAX_PARTIAL_BYTES and a valid UTF-8 prefix.
        assert!(out.len() <= MAX_PARTIAL_BYTES);
        // Round-tripping through str guarantees valid UTF-8 (panic otherwise).
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn mirror_to_session_inserts_when_session_exists() {
        let url = match std::env::var("DATABASE_URL") { Ok(u) => u, Err(_) => return };
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2).connect(&url).await.expect("connect");

        // Create a session with a known user_id (lookup is by user_id, not participants)
        let session_id = uuid::Uuid::new_v4();
        let agent_id = format!("test-agent-{}", &session_id.to_string()[..8]);
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, participants)
             VALUES ($1, $2, '999', 'telegram', ARRAY['999'])"
        ).bind(session_id).bind(&agent_id).execute(&db).await.expect("insert session");

        let found = super::mirror_to_session(&db, &agent_id, "telegram", "999", "hello from cron")
            .await.expect("mirror_to_session");
        assert!(found, "should return true when session exists");

        let (role, content, is_mirror): (String, String, bool) = sqlx::query_as(
            "SELECT role, content, is_mirror FROM messages WHERE session_id = $1 AND is_mirror = true"
        ).bind(session_id).fetch_one(&db).await.expect("fetch mirror row");

        assert_eq!(role, "assistant");
        assert_eq!(content, "hello from cron");
        assert!(is_mirror);
    }

    #[tokio::test]
    async fn mirror_to_session_returns_false_when_no_session() {
        let url = match std::env::var("DATABASE_URL") { Ok(u) => u, Err(_) => return };
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2).connect(&url).await.expect("connect");

        let found = super::mirror_to_session(
            &db, "nonexistent-agent", "telegram", "000", "nobody home"
        ).await.expect("mirror_to_session");
        assert!(!found, "should return false when no matching session");
    }

    #[test]
    fn compression_event_row_has_required_fields() {
        let _row = super::CompressionEventRow {
            segment_index: 1,
            first_live_message_id: None,
            summary: String::new(),
        };
    }

    #[test]
    fn messages_page_has_required_fields() {
        let _page = super::MessagesPage {
            messages: vec![],
            compression_events: vec![],
            has_more: false,
        };
    }
}

#[cfg(test)]
mod resolve_active_dm_session_tests {
    use super::*;

    #[test]
    fn dm_scope_keys_per_channel_peer() {
        assert_eq!(
            dm_scope_keys("alice", "telegram", "per-channel-peer"),
            ("alice", "telegram"),
        );
    }

    #[test]
    fn dm_scope_keys_shared_strips_channel() {
        assert_eq!(dm_scope_keys("alice", "telegram", "shared"), ("alice", "*"));
        assert_eq!(dm_scope_keys("alice", "discord", "per-peer"), ("alice", "*"));
    }

    #[test]
    fn dm_scope_keys_per_chat_strips_user() {
        assert_eq!(dm_scope_keys("alice", "telegram", "per-chat"), ("*", "telegram"));
    }

    #[test]
    fn dm_scope_keys_unknown_falls_back_to_per_channel_peer() {
        assert_eq!(
            dm_scope_keys("alice", "telegram", "garbage"),
            ("alice", "telegram"),
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_returns_none_when_no_session(pool: sqlx::PgPool) {
        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_returns_recent_done_session(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "done").await.unwrap();

        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert_eq!(got, Some((sid, Some(SessionStatus::Done))));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_skips_failed_session(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "failed").await.unwrap();

        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert!(got.is_none(), "failed sessions must NOT be reused");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_skips_interrupted_timeout_cancelled(pool: sqlx::PgPool) {
        for status in ["interrupted", "timeout", "cancelled"] {
            let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
            set_session_run_status(&pool, sid, status).await.unwrap();

            let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
                .await
                .unwrap();
            assert!(got.is_none(), "{status} sessions must NOT be reused");
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_returns_running_session(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "running").await.unwrap();

        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert_eq!(got, Some((sid, Some(SessionStatus::Running))));
    }
}

#[cfg(test)]
mod get_or_create_with_mode_tests {
    use super::*;
    use crate::ReentryMode;

    #[sqlx::test(migrations = "../../migrations")]
    async fn fresh_session_classified_as_new(pool: sqlx::PgPool) {
        let (sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert_eq!(mode, ReentryMode::NewSession);
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id = $1)")
            .bind(sid)
            .fetch_one(&pool).await.unwrap();
        assert!(exists);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn done_session_classified_as_new_turn(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "done").await.unwrap();

        let (got_sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert_eq!(got_sid, sid);
        assert_eq!(mode, ReentryMode::NewTurnAfterDone);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn running_session_classified_as_resume(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "running").await.unwrap();

        let (got_sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert_eq!(got_sid, sid);
        assert_eq!(mode, ReentryMode::ResumeRunning);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn failed_session_creates_fresh_one(pool: sqlx::PgPool) {
        let old = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, old, "failed").await.unwrap();

        let (got_sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer")
            .await
            .unwrap();
        assert_ne!(got_sid, old, "must create a NEW session, not reuse failed");
        assert_eq!(mode, ReentryMode::NewSession);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn interrupted_timeout_cancelled_create_fresh(pool: sqlx::PgPool) {
        for status in ["interrupted", "timeout", "cancelled"] {
            let old = create_new_session(&pool, "agent", &format!("u_{status}"), "telegram").await.unwrap();
            set_session_run_status(&pool, old, status).await.unwrap();

            let (got_sid, mode) = get_or_create_session(&pool, "agent", &format!("u_{status}"), "telegram", "per-channel-peer")
                .await
                .unwrap();
            assert_ne!(got_sid, old, "{status} must create a NEW session");
            assert_eq!(mode, ReentryMode::NewSession);
        }
    }
}

#[cfg(test)]
mod get_session_run_status_tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn returns_none_for_missing(pool: sqlx::PgPool) {
        let got = get_session_run_status(&pool, Uuid::new_v4()).await.unwrap();
        assert!(got.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn returns_none_for_null_status(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        let got = get_session_run_status(&pool, sid).await.unwrap();
        assert!(got.is_none(), "fresh session should have NULL run_status");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn returns_set_status(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "done").await.unwrap();
        let got = get_session_run_status(&pool, sid).await.unwrap();
        assert_eq!(got.as_deref(), Some("done"));
    }
}
