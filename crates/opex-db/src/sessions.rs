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

/// Machine-readable tag prefixing every synthetic tool result written for a tool
/// call interrupted before its result could be recorded (process restart or
/// interactive cancel). The dispatcher (durable-resumption Phase 3) matches this
/// prefix to require verify-before-redo for non-idempotent tools. Kept as the
/// leading substring of [`INTERRUPTED_TOOL_RESULT`].
pub const INTERRUPTED_VERIFY_TAG: &str = "[interrupted:verify]";

/// Canonical synthetic body for an interrupted tool call. Single source of truth
/// shared by the startup sweep, the per-id repair, and the read-path transcript
/// repair so the tag and wording stay consistent. Cause-agnostic so it reads
/// sensibly for both a process restart and an interactive cancel.
pub const INTERRUPTED_TOOL_RESULT: &str = "[interrupted:verify] This tool call was interrupted before its result was recorded; it may or may not have completed its side effect (file write, message send, code execution, etc.). Verify the current state before repeating the action.";

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct Session {
    pub id: uuid::Uuid,
    pub agent_id: String,
    pub user_id: String,
    pub channel: String,
    /// Per-chat/group/thread disambiguator (see `dm_scope_keys` doc). `None`
    /// for pre-migration rows and platforms with no chat concept.
    #[sqlx(default)]
    pub chat_scope: Option<String>,
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

/// Resolve `(user_id, channel, chat_scope)` lookup keys based on the agent's
/// DM scope.
///
/// Pure function — used by `get_or_create_session`, `resolve_active_dm_session`,
/// and the channel WS dispatcher (`SessionKey`) so all three derive the same
/// logical session identifier.
///
/// `chat_scope` is the per-chat/group/thread disambiguator threaded from the
/// incoming message's adapter context (e.g. Telegram `chat_id`, Discord
/// `"guild_id:channel_id"`, Slack channel id, Matrix `room_id`). It is
/// returned as-is (never collapsed to a sentinel) — `None` means "no chat
/// concept on this platform, or the caller couldn't supply one", and
/// degrades to a plain `(agent_id, user_id, channel)` match (the pre-fix
/// behaviour), never a panic.
///
/// IMPORTANT: unlike `user_id`/`channel`, `chat_scope` is NOT stored in a
/// dedicated eq-match sentinel like `"*"` — `dm_scope` variants that want to
/// ignore chat entirely return `None` for it, and the SQL layer treats a
/// `None` bind as "match rows with NULL chat_scope" (see
/// `resolve_active_dm_session` / `get_or_create_session`), which is exactly
/// the pre-migration data shape (existing rows have `chat_scope IS NULL`).
///
/// - `"per-channel-peer"` (default): `(user_id, channel, chat_scope)` —
///   distinct sessions per chat platform AND per chat/group for the same
///   user (T03 triage Point 5: previously collapsed ALL chats/groups on one
///   platform into a single session).
/// - `"shared"` / `"per-peer"`: `(user_id, "*", None)` — single
///   cross-channel DM; deliberately ignores both channel and chat_scope
///   (this mode explicitly wants ONE session regardless of platform/chat).
/// - `"per-chat"`: `("*", channel, chat_scope)` — group-chat sessions keyed
///   on the actual chat, not on the bare channel label (T03 triage Point 5:
///   previously used `channel` alone, so ALL users of ALL chats on a
///   platform collapsed into one session — "per-chat" isolation was
///   completely broken).
/// - Any other value falls back to `per-channel-peer` (matches the legacy
///   wildcard arm in `get_or_create_session`).
pub fn dm_scope_keys<'a>(
    user_id: &'a str,
    channel: &'a str,
    dm_scope: &str,
    chat_scope: Option<&'a str>,
) -> (&'a str, &'a str, Option<&'a str>) {
    match dm_scope {
        "shared" | "per-peer" => (user_id, "*", None),
        "per-chat" => ("*", channel, chat_scope),
        _ => (user_id, channel, chat_scope),
    }
}

/// Look up the most recent active DM session for `(agent, user, channel,
/// chat_scope)` after applying `dm_scope`, or `None` if none qualifies.
///
/// "Active" means `last_message_at > now() - 4h`, regardless of `run_status`
/// (R-CONTINUITY fix). Soft-terminal sessions (`'failed'`, `'interrupted'`,
/// `'timeout'`, `'cancelled'`) are NO LONGER excluded: a live user message
/// reuses them (see `get_or_create_session`), so the mirror — which must land
/// in exactly the session a live message would — has to resolve them too.
/// Re-entry repairs orphan tool calls / streaming rows, so reuse is safe.
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
    chat_scope: Option<&str>,
) -> Result<Option<(Uuid, Option<SessionStatus>)>> {
    let (eff_user, eff_channel, eff_chat_scope) = dm_scope_keys(user_id, channel, dm_scope, chat_scope);
    // `chat_scope IS NOT DISTINCT FROM $4`: NULL-safe equality so a `None`
    // bind matches rows with `chat_scope IS NULL` (the pre-migration shape
    // and the graceful "platform has no chat concept" degrade), while a
    // `Some(x)` bind matches only rows with that exact chat_scope.
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, run_status FROM sessions \
         WHERE agent_id = $1 AND user_id = $2 AND channel = $3 \
           AND chat_scope IS NOT DISTINCT FROM $4 \
           AND last_message_at > now() - interval '4 hours' \
         ORDER BY last_message_at DESC LIMIT 1",
    )
    .bind(agent_id)
    .bind(eff_user)
    .bind(eff_channel)
    .bind(eff_chat_scope)
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
/// `dm_scope` controls session isolation (see `dm_scope_keys`). `chat_scope`
/// is the per-chat/group disambiguator (see `dm_scope_keys` doc) — `None`
/// degrades to the pre-fix `(agent_id, user_id, channel)` match.
pub async fn get_or_create_session(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    channel: &str,
    dm_scope: &str,
    chat_scope: Option<&str>,
) -> Result<(Uuid, crate::ReentryMode)> {
    let (eff_user, eff_channel, eff_chat_scope) = dm_scope_keys(user_id, channel, dm_scope, chat_scope);

    // Advisory lock keyed on (agent_id, user_id, channel, chat_scope) hash
    // prevents concurrent transactions from both inserting when no session
    // exists. The CTE alone is NOT sufficient — PostgreSQL snapshot isolation
    // lets two concurrent CTEs both see `existing` as empty and both INSERT.
    // The advisory lock serializes access. `chat_scope` defaults to the empty
    // string in the hash input (hashtext requires non-NULL text) — this only
    // affects lock-key collision odds, not correctness, since the CTE's own
    // NULL-safe WHERE is the actual correctness guard.
    let mut tx = db.begin().await?;

    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1 || $2 || $3 || $4))")
        .bind(agent_id)
        .bind(eff_user)
        .bind(eff_channel)
        .bind(eff_chat_scope.unwrap_or(""))
        .execute(&mut *tx)
        .await?;

    // Same filter as resolve_active_dm_session: reuse the most-recent session
    // for this (agent,user,channel,chat_scope) within the 4h window
    // REGARDLESS of soft-terminal status (R-CONTINUITY fix). Previously
    // failed/interrupted/timeout/cancelled rows were excluded, so any
    // non-clean turn-end silently forked a fresh, context-less session on the
    // user's next message — the dominant cause of "the agent forgot the
    // conversation", especially on channels (Telegram/Discord) which never
    // supply a resume_session_id. Re-entry repairs orphan tool calls +
    // streaming rows (bootstrap calls cleanup_session_streaming_messages +
    // insert_synthetic_tool_results), so reusing a soft-terminal row no
    // longer "poisons" the next turn. Explicit "New Chat" still uses
    // create_new_session for a guaranteed-fresh row. The `was_new` boolean
    // disambiguates existing-vs-new for ReentryMode.
    //
    // `chat_scope IS NOT DISTINCT FROM $4` (NULL-safe eq): a `None` bind
    // matches only NULL rows (pre-migration shape / no-chat-concept
    // platforms); `Some(x)` matches only that exact chat_scope. The INSERT
    // stamps `chat_scope` from the same bind so a freshly created row is
    // immediately findable by the same predicate on the next call.
    let row: (Uuid, Option<String>, bool) = sqlx::query_as(
        "WITH existing AS ( \
           SELECT id, run_status FROM sessions \
           WHERE agent_id = $1 AND user_id = $2 AND channel = $3 \
             AND chat_scope IS NOT DISTINCT FROM $4 \
             AND last_message_at > now() - interval '4 hours' \
           ORDER BY last_message_at DESC LIMIT 1 \
         ), inserted AS ( \
           INSERT INTO sessions (agent_id, user_id, channel, chat_scope, participants) \
           SELECT $1, $2, $3, $4, ARRAY[$1::text] \
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
    .bind(eff_chat_scope)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let (id, run_status_str, was_new) = row;
    let mode = if was_new {
        crate::ReentryMode::NewSession
    } else {
        let parsed = run_status_str.as_deref().and_then(SessionStatus::parse);
        match parsed {
            // Soft-terminal reuse (failed/interrupted/timeout/cancelled): the
            // user is continuing the conversation, so treat it like an explicit
            // resume — claim_session_for_reentry allows any→running and the
            // loop detector warms from the prior timeline. Mirrors the resume
            // path in context_builder (`Some(_) => ExplicitResume`).
            Some(s) if s.is_terminal() && s != SessionStatus::Done => {
                crate::ReentryMode::ExplicitResume
            }
            // FOUND row (was_new = false) with no run_status yet — created but
            // its first turn never set a status, or a rapid re-entry before the
            // first turn completed. This is a REUSE, not a brand-new session, so
            // it must NOT classify as NewSession. `ReentryMode::classify(None)`
            // returns NewSession (correct only for the was_new = true insert
            // case), so map the found-NULL case to a continuation here.
            None => crate::ReentryMode::NewTurnAfterDone,
            other => crate::ReentryMode::classify(other),
        }
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
    messages: &[opex_types::Message],
) -> anyhow::Result<()> {
    use chrono::Utc;
    for (i, msg) in messages.iter().enumerate() {
        let role: &str = match msg.role {
            opex_types::MessageRole::System    => "system",
            opex_types::MessageRole::User      => "user",
            opex_types::MessageRole::Assistant => "assistant",
            opex_types::MessageRole::Tool      => "tool",
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
    parallel_batch_id: Option<opex_types::ids::ParallelBatchId>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, tool_calls, tool_call_id, agent_id, thinking_blocks, parent_message_id, parallel_batch_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
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
    .bind(parallel_batch_id.map(|b| b.as_uuid()))
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

/// Prepend `prefix` to an existing message row's `content` column.
///
/// Used by `BackgroundMediaTask::deliver_to_channel` to prepend a
/// `__file__:{json}\n` marker so the UI inline parser
/// (`chat-history.ts:196`) renders the channel-delivered media as an inline
/// image / audio / video element when the session is reloaded in the web UI.
///
/// **Idempotency:** safe against retries ONLY when the prefix is the same on
/// every call. Callers MUST NOT call this twice with different prefixes for
/// the same row, or the content will be doubly-prefixed.
///
/// **No-op on missing row:** matches the [`update_message_step_id`] contract.
/// The persist insert spawns detached, so this prepend may fire while the
/// insert is still pending — a 0-row UPDATE is not an error.
pub async fn prepend_message_content(
    db: &PgPool,
    id: Uuid,
    prefix: &str,
) -> Result<()> {
    sqlx::query("UPDATE messages SET content = $1 || content WHERE id = $2")
        .bind(prefix)
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
    // Match the doc-comment semantics in SQL: only `running` or NULL may
    // transition. Previously the WHERE was `IS DISTINCT FROM 'done'`, which
    // also permitted `failed → done`, `interrupted → failed`, etc. — those
    // are exactly the terminal→terminal jumps we want to block.
    sqlx::query(
        "UPDATE sessions SET run_status = $1 \
         WHERE id = $2 \
           AND (run_status IS NULL OR run_status = 'running')"
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
/// safe: `cleanup_session_terminated` (used by `SessionLifecycleGuard`)
/// performs its step-1 claim with `WHERE run_status = 'running'`, so a
/// completed-then-reclaimed session cannot be accidentally set to `'failed'`
/// by a stale guard.
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

/// Mode-aware variant of `claim_session_running`. Sets `run_status = 'running'`
/// only when the transition is consistent with `mode`:
///
/// - `NewSession`: row was just inserted; allow `NULL → running`.
/// - `NewTurnAfterDone`: allow `done → running` (chat continuation).
/// - `ResumeRunning`: idempotent self-update; allow `running → running`.
/// - `ExplicitResume`: user explicitly opened a session via UI / fork /
///   `resume_session_id`. Status may be soft-terminal — allow ANY → running.
///
/// Returns `Ok(true)` when the row was updated, `Ok(false)` when the row
/// is missing or in an incompatible status. The narrow TOCTOU race (status
/// flipped between resolve and claim) is handled by `claim_session_with_retry`.
pub async fn claim_session_for_reentry(
    db: &PgPool,
    session_id: Uuid,
    mode: crate::ReentryMode,
) -> Result<bool> {
    let allowed_from = match mode {
        crate::ReentryMode::NewSession => "(run_status IS NULL)",
        crate::ReentryMode::NewTurnAfterDone => "(run_status = 'done')",
        crate::ReentryMode::ResumeRunning => "(run_status = 'running')",
        crate::ReentryMode::ExplicitResume => "TRUE",
    };
    // Allowed-from is a literal SQL fragment from a closed match arm —
    // never user input — so it cannot be SQL-injected.
    let q = format!(
        "UPDATE sessions SET run_status = 'running' WHERE id = $1 AND {allowed_from}",
    );
    let rows = sqlx::query(&q).bind(session_id).execute(db).await?.rows_affected();
    Ok(rows > 0)
}

/// Convenience: claim with one retry on race. If the initial claim fails
/// (status changed between resolve and claim), re-fetch status and retry
/// using `ExplicitResume` mode (any → running). Without retry, the user's
/// message would be lost on a narrow but real race window.
pub async fn claim_session_with_retry(
    db: &PgPool,
    session_id: Uuid,
    initial_mode: crate::ReentryMode,
) -> Result<bool> {
    if claim_session_for_reentry(db, session_id, initial_mode).await? {
        return Ok(true);
    }
    tracing::warn!(
        %session_id,
        ?initial_mode,
        "claim_session_for_reentry raced; retrying with ExplicitResume",
    );
    // If the row was deleted between resolve and now, no point retrying.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = $1)",
    )
    .bind(session_id)
    .fetch_one(db)
    .await?;
    if !exists {
        return Ok(false);
    }
    claim_session_for_reentry(db, session_id, crate::ReentryMode::ExplicitResume).await
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

/// Single cleanup path for all session terminations.
///
/// I1 invariant: watchdog, startup-cleanup, and SessionLifecycleGuard::Drop
/// all call this function. Idempotent — returns `Ok(false)` when the session
/// was already terminal (another path won the race).
///
/// All four steps run in one transaction so a connection failure between
/// claim and timeline insert cannot leave the session in a half-cleaned state
/// (R-CRIT-2).
pub async fn cleanup_session_terminated(
    db: &PgPool,
    session_id: Uuid,
    target_status: &str,
    reason: &str,
) -> Result<bool> {
    let mut tx = db.begin().await?;

    // 1. Atomic claim — only proceed if still 'running'.
    let claimed = sqlx::query(
        "UPDATE sessions SET run_status = $1 WHERE id = $2 AND run_status = 'running'"
    )
    .bind(target_status)
    .bind(session_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed == 0 {
        tx.rollback().await.ok();
        return Ok(false);
    }

    // 2. Preserve partial text in streaming messages (UPDATE, not DELETE).
    sqlx::query(
        "UPDATE messages SET status = 'interrupted',
         content = COALESCE(NULLIF(content, ''), '[interrupted]')
         WHERE session_id = $1 AND status = 'streaming'"
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await?;

    // 3. Synthetic tool results for orphan tool_calls.
    //    Bare function name — we are already inside `sessions` module, so no
    //    `crate::sessions::` prefix.
    insert_synthetic_tool_results_tx(&mut tx, session_id).await?;

    // 4. Timeline event. Heartbeat side-effect inside log_event_tx is a no-op here
    //    because run_status is no longer 'running' inside this tx.
    //    `session_timeline` is a sibling module in the same crate
    //    (opex-db/src/lib.rs declares both), so `crate::session_timeline::...`
    //    is the right path.
    let payload = serde_json::json!({ "reason": reason });
    crate::session_timeline::log_event_tx(&mut tx, session_id, target_status, Some(&payload)).await?;

    tx.commit().await?;
    Ok(true)
}

/// Refresh `activity_at` heartbeat (debounced to 10 s resolution, gated by
/// `run_status = 'running'`). Called from `upsert_streaming_append` every ~2 s
/// during streaming; the debounce keeps the UPDATE near-free under load and
/// the run-status guard prevents resurrection of terminal sessions.
pub async fn touch_session_activity(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE sessions SET activity_at = NOW()
         WHERE id = $1
           AND run_status = 'running'
           AND (activity_at IS NULL OR activity_at < NOW() - INTERVAL '10 seconds')"
    )
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Find sessions stuck in 'running' with no activity beyond their per-agent
/// threshold. Agents missing from the map fall back to `default_secs`.
/// Returns Vec<(session_id, agent_id, idle_seconds)>.
pub async fn find_stale_running_sessions_per_agent(
    db: &PgPool,
    agent_inactivity: &std::collections::HashMap<String, i64>,
    default_secs: i64,
) -> Result<Vec<(Uuid, String, i64)>> {
    if agent_inactivity.is_empty() {
        // All agents fall back to default.
        let rows = sqlx::query_as::<_, (Uuid, String, i64)>(
            "SELECT id, agent_id,
                    EXTRACT(EPOCH FROM (NOW() - COALESCE(activity_at, last_message_at)))::BIGINT
             FROM sessions
             WHERE run_status = 'running'
               AND COALESCE(activity_at, last_message_at)
                   < NOW() - make_interval(secs => $1)"
        )
        .bind(default_secs)
        .fetch_all(db)
        .await?;
        return Ok(rows);
    }

    // Build VALUES list for the WITH clause.
    let mut qb = sqlx::QueryBuilder::new(
        "WITH agent_thresholds(agent_id, secs) AS (VALUES "
    );
    let mut first = true;
    for (agent, secs) in agent_inactivity {
        if !first { qb.push(", "); }
        qb.push("(").push_bind(agent).push("::TEXT, ").push_bind(*secs).push("::BIGINT)");
        first = false;
    }
    qb.push(") SELECT s.id, s.agent_id,
                     EXTRACT(EPOCH FROM (NOW() - COALESCE(s.activity_at, s.last_message_at)))::BIGINT
              FROM sessions s
              LEFT JOIN agent_thresholds t USING (agent_id)
              WHERE s.run_status = 'running'
                AND COALESCE(s.activity_at, s.last_message_at)
                    < NOW() - make_interval(secs => COALESCE(t.secs, ");
    qb.push_bind(default_secs);
    qb.push("))");

    let rows = qb.build_query_as::<(Uuid, String, i64)>().fetch_all(db).await?;
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

/// Atomically claim a crashed autonomous (cron) session for re-drive: flip
/// `interrupted` → `running` AND charge one retry, enforcing the budget in the
/// SAME statement. Returns the new `retry_count` on success, or `None` when
/// another worker already claimed it (status no longer `interrupted`) or the
/// budget is exhausted (`retry_count >= max_retries`). This single-statement
/// claim is the per-session mutual exclusion for the startup resumer.
///
/// Mirror of [`increment_retry_count`] for the re-drive case — that one matches
/// only `running`/`done`, so it cannot claim an interrupted session.
pub async fn claim_redrive(db: &PgPool, session_id: Uuid, max_retries: i32) -> Result<Option<i32>> {
    let row: Option<(i32,)> = sqlx::query_as(
        "UPDATE sessions SET retry_count = retry_count + 1, run_status = 'running' \
         WHERE id = $1 AND run_status IN ('interrupted', 'done') AND retry_count < $2 \
         RETURNING retry_count",
    )
    .bind(session_id)
    .bind(max_retries)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(c,)| c))
}

/// Delete orphaned tool-result rows: `role='tool'` messages whose `tool_call_id`
/// is declared by NO `role='assistant'` message in the same session.
///
/// The DB-level counterpart of the read-path `drop_orphan_tool_results` filter.
/// A crash can commit a tool-result row while losing its parent assistant row
/// (the two persist via independent detached tasks — see `pipeline::execute`).
/// Filtering on read keeps the transcript valid, but the dangling rows otherwise
/// accumulate forever; this sweep removes them. Returns the number deleted.
///
/// Safe by construction: the synthetic `[interrupted:verify]` rows are NOT
/// orphans — they carry the `tool_call_id` of a declared (dangling) assistant
/// tool_call, so a matching assistant exists and they are preserved.
/// `jsonb_typeof = 'array'` guards against malformed `tool_calls` JSON.
pub async fn sweep_orphan_tool_results(db: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM messages t \
         WHERE t.role = 'tool' \
           AND t.tool_call_id IS NOT NULL \
           AND NOT EXISTS ( \
             SELECT 1 FROM messages a \
             CROSS JOIN LATERAL jsonb_array_elements(a.tool_calls) elem \
             WHERE a.session_id = t.session_id \
               AND a.role = 'assistant' \
               AND a.tool_calls IS NOT NULL \
               AND jsonb_typeof(a.tool_calls) = 'array' \
               AND elem->>'id' = t.tool_call_id \
           )",
    )
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Stamp the originating channel `chat_id` on a session (idempotent). Called on
/// each channel turn so an interrupted interactive `/goal` can be channel-pushed
/// (e.g. Telegram) instead of only surfacing in the UI bell. No-op for web
/// sessions (they have no chat_id, so this is never called for them).
pub async fn set_session_chat_id(db: &PgPool, session_id: Uuid, chat_id: i64) -> Result<()> {
    sqlx::query("UPDATE sessions SET chat_id = $2 WHERE id = $1")
        .bind(session_id)
        .bind(chat_id)
        .execute(db)
        .await?;
    Ok(())
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

/// Like [`get_last_user_message`] but also returns the row id, so the retry
/// path can scope its DELETE to the EXACT captured message instead of a
/// re-evaluated `ORDER BY created_at DESC LIMIT 1` subquery — which, if two
/// retries raced, could resolve to (and delete) an OLDER user turn (F031).
pub async fn get_last_user_message_with_id(
    db: &PgPool,
    session_id: Uuid,
) -> Result<Option<(Uuid, String)>> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, content FROM messages \
         WHERE session_id = $1 AND role = 'user' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
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
///
/// Standalone variant — opens its own transaction and commits on success.
/// Use [`insert_synthetic_tool_results_tx`] when already inside a transaction.
pub async fn insert_synthetic_tool_results(db: &PgPool, session_id: Uuid) -> Result<usize> {
    let mut tx = db.begin().await?;
    let n = insert_synthetic_tool_results_tx(&mut tx, session_id).await?;
    tx.commit().await?;
    Ok(n)
}

/// In-transaction variant — runs all queries through the passed transaction.
/// Used by `cleanup_session_terminated` (Task 4) to keep multi-step cleanup
/// atomic. Standalone callers should use [`insert_synthetic_tool_results`].
pub async fn insert_synthetic_tool_results_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: Uuid,
) -> Result<usize> {
    // Find assistant messages with tool_calls that have no matching tool result
    let assistant_msgs = sqlx::query_as::<_, (Uuid, serde_json::Value)>(
        "SELECT id, tool_calls FROM messages
         WHERE session_id = $1 AND role = 'assistant'
           AND tool_calls IS NOT NULL AND jsonb_array_length(tool_calls) > 0
         ORDER BY created_at"
    )
    .bind(session_id)
    .fetch_all(&mut **tx)
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
    .fetch_all(&mut **tx)
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
                .bind(INTERRUPTED_TOOL_RESULT)
                .bind(*call_id);
        }

        let result = q.execute(&mut **tx).await?;
        inserted += result.rows_affected() as usize;
    }
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
        .bind(INTERRUPTED_TOOL_RESULT)
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
    // Step 0 (R-LOOP fix): repair orphaned `status='streaming'` rows that belong
    // to sessions which are NOT 'running' (terminal `done`/`failed`/… or NULL).
    //
    // The per-session repair below uses `cleanup_session_terminated`, whose
    // step-1 claim is `WHERE run_status = 'running'`. For a session that is
    // already terminal yet still carries a leftover streaming row (e.g. the SSE
    // converter was killed after the engine's `guard.done()` but before
    // `finalize_streaming_message` deleted the placeholder), that claim matches
    // 0 rows and rolls back — the streaming row is never cleared. Such a row
    // keeps matching the batch SELECT every iteration, so the main.rs wrapper
    // loops forever and the gateway never binds its listener (total outage).
    //
    // Clearing these rows directly here (idempotent — they become 'interrupted'
    // so they stop matching) guarantees the batch loop converges. `IS DISTINCT
    // FROM 'running'` also covers NULL run_status (old pre-run_status rows).
    if let Err(e) = sqlx::query(
        "UPDATE messages SET status = 'interrupted',
                content = COALESCE(NULLIF(content, ''), '[interrupted]')
         WHERE status = 'streaming'
           AND session_id IN (
               SELECT id FROM sessions WHERE run_status IS DISTINCT FROM 'running'
           )"
    )
    .execute(db)
    .await
    {
        tracing::warn!(error = %e, "failed to repair orphaned streaming rows on terminal sessions");
    }

    // Find sessions that were 'running' when the process died (batched). After
    // step 0, the only sessions still carrying a 'streaming' row are 'running'
    // ones, so this single predicate covers both shapes the old `OR EXISTS`
    // clause did — without re-matching terminal sessions forever.
    let interrupted = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM sessions WHERE run_status = 'running' LIMIT 100"
    )
    .fetch_all(db)
    .await?;

    let count = interrupted.len();
    if count > 0 {
        tracing::info!(count, "cleaning up interrupted sessions");
    }

    for session_id in &interrupted {
        if let Err(e) = cleanup_session_terminated(
            db, *session_id, "interrupted", "crash_recovery"
        ).await {
            tracing::warn!(error = %e, session_id = %session_id, "startup cleanup failed");
        }
    }

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

/// Last-resort safety-net: forcibly mark any 'running' session idle longer
/// than `threshold_secs` as 'interrupted'. Called from startup-cleanup.
pub async fn finalize_truly_stale_sessions(
    db: &PgPool,
    threshold_secs: i64,
) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE sessions SET run_status = 'interrupted'
         WHERE run_status = 'running'
           AND COALESCE(activity_at, last_message_at) < NOW() - make_interval(secs => $1)"
    )
    .bind(threshold_secs)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
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

/// Find the active session for a user+agent+channel(+chat_scope) pair (last
/// 4 hours). Used by the channel slash commands (`/status`, `/new`, `/reset`,
/// `/compact`, ...) — see `agent::pipeline::commands`.
///
/// Delegates to `dm_scope_keys` so this stays consistent with
/// `get_or_create_session` / `resolve_active_dm_session` (previously had its
/// own inline copy of the scope-collapsing match, which had silently drifted
/// out of sync with the chat_scope fix — T03 triage Point 5).
pub async fn find_active_session(
    db: &PgPool,
    agent_id: &str,
    user_id: &str,
    channel: &str,
    dm_scope: &str,
    chat_scope: Option<&str>,
) -> Result<Option<Uuid>> {
    let (eff_user, eff_channel, eff_chat_scope) = dm_scope_keys(user_id, channel, dm_scope, chat_scope);

    let row = sqlx::query(
        "SELECT id FROM sessions \
         WHERE agent_id = $1 AND user_id = $2 AND channel = $3 \
           AND chat_scope IS NOT DISTINCT FROM $4 \
           AND last_message_at > now() - interval '4 hours' \
         ORDER BY last_message_at DESC LIMIT 1",
    )
    .bind(agent_id)
    .bind(eff_user)
    .bind(eff_channel)
    .bind(eff_chat_scope)
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
/// If `ui_event_tx` is provided, broadcasts a `session_updated` event on success.
pub async fn add_participant(
    db: &PgPool,
    session_id: Uuid,
    agent_name: &str,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
) -> Result<Vec<String>> {
    let row = sqlx::query(
        "UPDATE sessions SET participants = array_append(participants, $2) \
         WHERE id = $1 AND NOT ($2 = ANY(participants)) \
         RETURNING participants"
    )
    .bind(session_id)
    .bind(agent_name)
    .fetch_optional(db)
    .await?;

    let participants = if let Some(r) = row {
        let p: Vec<String> = r.get("participants");
        // Broadcast to all participants so their sidebars/badges refresh
        if let Some(tx) = ui_event_tx {
            for participant in &p {
                let event = serde_json::json!({
                    "type": "session_updated",
                    "agent": participant,
                    "session_id": session_id.to_string(),
                });
                let _ = tx.send(event.to_string());
            }
        }
        p
    } else {
        // Agent was already a participant — return current list
        let r = sqlx::query("SELECT participants FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(db)
            .await?;
        r.get("participants")
    };

    Ok(participants)
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

/// Append a delivery-mirror record to the active DM session for the given
/// agent + channel + participant.
///
/// Delegates session lookup to `resolve_active_dm_session` so mirrors land
/// in exactly the session a live Telegram message would land in. This means:
/// - Soft-terminal sessions (`failed`/`interrupted`/`timeout`/`cancelled`)
///   within the 4h window ARE reused now (R-CONTINUITY) — a live message
///   would continue them, so the mirror lands there too for consistency.
/// - Stale sessions older than the 4h horizon are skipped — the cron should
///   not silently resurrect a closed conversation.
///
/// `dm_scope` is hard-coded to `"per-channel-peer"` because callers (cron,
/// heartbeat) don't have the agent config in scope. Agents using `"shared"`
/// or `"per-chat"` scopes won't be reached by mirrors — known limitation
/// inherited from the legacy implementation; tracked separately.
///
/// `chat_scope`: the per-chat/group disambiguator for the target DM (see
/// `dm_scope_keys` doc). Callers pushing to a specific chat (cron announce
/// targets carry a `chat_id`) should pass it through so the mirror lands in
/// the SAME session a live message in that chat would resolve to — matching
/// `T03` triage Point 5's fix for the live-message path. `None` degrades to
/// matching only chat-scope-less (pre-migration/NULL) rows, same as before
/// this parameter existed.
///
/// Returns `Ok(true)` if a matching session was found and the record inserted.
/// Returns `Ok(false)` if no active DM session exists. Never fails fatally —
/// callers fire-and-forget via `tokio::spawn`.
pub async fn mirror_to_session(
    db: &PgPool,
    agent_id: &str,
    channel: &str,
    participant_id: &str,
    chat_scope: Option<&str>,
    text: &str,
) -> anyhow::Result<bool> {
    // Per-chat group sessions use `user_id = "*"` as a sentinel; they are
    // not DM sessions and must never receive mirror records (cron deliveries
    // would leak into group chats). Skip early before touching the DB —
    // covered by `mirror_skips_per_chat_group_sessions` integration test.
    if participant_id == "*" {
        return Ok(false);
    }

    let resolved = resolve_active_dm_session(
        db,
        agent_id,
        participant_id,
        channel,
        "per-channel-peer",
        chat_scope,
    )
    .await?;

    let session_id = match resolved {
        Some((id, _status)) => id,
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

/// Insert a session_timeline record for a compression boundary.
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
        "INSERT INTO session_timeline (session_id, event_type, payload)
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
               FROM session_timeline
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn mirror_to_session_inserts_when_session_exists(pool: sqlx::PgPool) {
        // Create a session with a known user_id (lookup is by user_id).
        let session_id = uuid::Uuid::new_v4();
        let agent_id = format!("test-agent-{}", &session_id.to_string()[..8]);
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, participants)
             VALUES ($1, $2, '999', 'telegram', ARRAY['999'])"
        ).bind(session_id).bind(&agent_id).execute(&pool).await.expect("insert session");

        let found = super::mirror_to_session(&pool, &agent_id, "telegram", "999", None, "hello from cron")
            .await.expect("mirror_to_session");
        assert!(found, "should return true when session exists");

        let (role, content, is_mirror): (String, String, bool) = sqlx::query_as(
            "SELECT role, content, is_mirror FROM messages WHERE session_id = $1 AND is_mirror = true"
        ).bind(session_id).fetch_one(&pool).await.expect("fetch mirror row");

        assert_eq!(role, "assistant");
        assert_eq!(content, "hello from cron");
        assert!(is_mirror);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mirror_to_session_returns_false_when_no_session(pool: sqlx::PgPool) {
        let found = super::mirror_to_session(
            &pool, "nonexistent-agent", "telegram", "000", None, "nobody home"
        ).await.expect("mirror_to_session");
        assert!(!found, "should return false when no matching session");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mirror_targets_same_session_as_get_or_create(pool: sqlx::PgPool) {
        // get_or_create creates a session for (agent, alice, telegram). A cron
        // mirror to the same key MUST land in that exact session.
        let (sid, _) = super::get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await.unwrap();

        let inserted = super::mirror_to_session(&pool, "agent", "telegram", "alice", None, "hello cron")
            .await.unwrap();
        assert!(inserted);

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE session_id = $1 AND is_mirror = true"
        ).bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mirror_reuses_failed_session(pool: sqlx::PgPool) {
        // R-CONTINUITY: a live message now reuses a failed session within 4h,
        // so the mirror must land there too (it tracks where a live message
        // would go). Previously the mirror skipped soft-terminal sessions.
        let sid = super::create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        super::set_session_run_status(&pool, sid, "failed").await.unwrap();

        let inserted = super::mirror_to_session(&pool, "agent", "telegram", "alice", None, "hello cron")
            .await.unwrap();
        assert!(inserted, "mirror must write into the reused (soft-terminal) session");
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE session_id = $1 AND is_mirror = true"
        ).bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mirror_skips_done_older_than_4h(pool: sqlx::PgPool) {
        let sid = super::create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        super::set_session_run_status(&pool, sid, "done").await.unwrap();
        // Backdate last_message_at past the 4h horizon.
        sqlx::query("UPDATE sessions SET last_message_at = now() - interval '5 hours' WHERE id = $1")
            .bind(sid).execute(&pool).await.unwrap();

        let inserted = super::mirror_to_session(&pool, "agent", "telegram", "alice", None, "hello cron")
            .await.unwrap();
        assert!(!inserted, "mirror must not resurrect a stale session");
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn touch_session_activity_is_debounced(pool: sqlx::PgPool) {
        let session_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, activity_at)
             VALUES ($1, 'a', 'u', 'web', 'running', NOW())"
        ).bind(session_id).execute(&pool).await.unwrap();

        let before: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT activity_at FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        super::touch_session_activity(&pool, session_id).await.unwrap();

        let after: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT activity_at FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(before, after, "debounce must skip update within 10s");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_synthetic_tool_results_tx_runs_in_provided_tx(pool: sqlx::PgPool) {
        let session_id = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, status)
             VALUES ($1, 'assistant', '', $2::jsonb, 'complete')"
        )
        .bind(session_id)
        .bind(serde_json::json!([{"id":"call_x","name":"t","arguments":{}}]))
        .execute(&pool).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        let n = super::insert_synthetic_tool_results_tx(&mut tx, session_id).await.unwrap();
        assert_eq!(n, 1);
        tx.rollback().await.unwrap();  // verify rollback works

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE session_id = $1 AND role = 'tool'"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "rollback must discard synthetic results");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn claim_redrive_flips_interrupted_to_running_and_charges_retry(pool: sqlx::PgPool) {
        let sid = super::create_new_session(&pool, "a", "u", "CRON").await.unwrap();
        super::set_session_run_status(&pool, sid, "interrupted").await.unwrap();

        let got = super::claim_redrive(&pool, sid, 3).await.unwrap();
        assert_eq!(got, Some(1), "first claim flips status and charges retry 1");

        let status: String = sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
            .bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(status, "running", "claim flips interrupted -> running");

        // A now-running session can no longer be claimed (single-claim guarantee).
        let again = super::claim_redrive(&pool, sid, 3).await.unwrap();
        assert_eq!(again, None, "a running session cannot be re-claimed for re-drive");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn claim_redrive_also_claims_done_session(pool: sqlx::PgPool) {
        // A cron goal that crashed BETWEEN turns leaves the session 'done' (the
        // last turn finalized) while the goal is still active — the in-memory
        // driver is lost. Re-drive must claim it, not only 'interrupted' ones.
        let sid = super::create_new_session(&pool, "a", "u", "CRON").await.unwrap();
        super::set_session_run_status(&pool, sid, "done").await.unwrap();

        let got = super::claim_redrive(&pool, sid, 3).await.unwrap();
        assert_eq!(got, Some(1), "a 'done' cron session (driver lost between turns) is claimable");

        let status: String = sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
            .bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(status, "running", "claim flips done -> running");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn claim_redrive_respects_budget(pool: sqlx::PgPool) {
        let sid = super::create_new_session(&pool, "a", "u", "CRON").await.unwrap();
        super::set_session_run_status(&pool, sid, "interrupted").await.unwrap();
        sqlx::query("UPDATE sessions SET retry_count = 3 WHERE id = $1")
            .bind(sid).execute(&pool).await.unwrap();

        let got = super::claim_redrive(&pool, sid, 3).await.unwrap();
        assert_eq!(got, None, "budget exhausted (retry_count >= max_retries) -> no claim");

        let status: String = sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
            .bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(status, "interrupted", "exhausted claim leaves status untouched");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn sweep_orphan_tool_results_deletes_only_undeclared(pool: sqlx::PgPool) {
        let sid = super::create_new_session(&pool, "a", "u", "web").await.unwrap();

        // Assistant declares tool_call "X" (note: serde-transparent ToolCallId → "id" is a plain string).
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls) \
             VALUES ($1, 'assistant', '', '[{\"id\":\"X\",\"name\":\"t\",\"arguments\":{}}]'::jsonb)",
        )
        .bind(sid).execute(&pool).await.unwrap();
        // Declared result for X → keep.
        sqlx::query("INSERT INTO messages (session_id, role, content, tool_call_id) VALUES ($1, 'tool', 'ok', 'X')")
            .bind(sid).execute(&pool).await.unwrap();
        // Orphan result for Y (no assistant declares it) → delete.
        sqlx::query("INSERT INTO messages (session_id, role, content, tool_call_id) VALUES ($1, 'tool', 'lost', 'Y')")
            .bind(sid).execute(&pool).await.unwrap();
        // A synthetic [interrupted:verify] result for a SEPARATE declared call "Z" → keep
        // (it carries the tool_call_id of a declared dangling call, so it is not an orphan).
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls) \
             VALUES ($1, 'assistant', '', '[{\"id\":\"Z\",\"name\":\"t\",\"arguments\":{}}]'::jsonb)",
        )
        .bind(sid).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO messages (session_id, role, content, tool_call_id) VALUES ($1, 'tool', $2, 'Z')")
            .bind(sid).bind(super::INTERRUPTED_TOOL_RESULT).execute(&pool).await.unwrap();

        let deleted = super::sweep_orphan_tool_results(&pool).await.unwrap();
        assert_eq!(deleted, 1, "only the undeclared orphan (Y) is swept");

        let remaining: Vec<String> = sqlx::query_scalar(
            "SELECT tool_call_id FROM messages WHERE session_id = $1 AND role = 'tool' ORDER BY tool_call_id",
        )
        .bind(sid).fetch_all(&pool).await.unwrap();
        assert_eq!(remaining, vec!["X".to_string(), "Z".to_string()], "declared + interrupted-verify results survive");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn set_session_chat_id_roundtrips(pool: sqlx::PgPool) {
        let sid = super::create_new_session(&pool, "a", "u", "telegram").await.unwrap();
        let before: Option<i64> = sqlx::query_scalar("SELECT chat_id FROM sessions WHERE id = $1")
            .bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(before, None, "new session has no chat_id");

        super::set_session_chat_id(&pool, sid, 123_456).await.unwrap();
        let after: Option<i64> = sqlx::query_scalar("SELECT chat_id FROM sessions WHERE id = $1")
            .bind(sid).fetch_one(&pool).await.unwrap();
        assert_eq!(after, Some(123_456), "chat_id is persisted");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_session_terminated_is_idempotent(pool: sqlx::PgPool) {
        let session_id = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        super::set_session_run_status(&pool, session_id, "running").await.unwrap();

        let first = super::cleanup_session_terminated(&pool, session_id, "timeout", "r1")
            .await.unwrap();
        assert!(first, "first call must claim");

        let second = super::cleanup_session_terminated(&pool, session_id, "failed", "r2")
            .await.unwrap();
        assert!(!second, "second call must return false (already terminal)");

        let status: String = sqlx::query_scalar(
            "SELECT run_status FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(status, "timeout", "first claim wins, second is no-op");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_session_terminated_preserves_partial_text(pool: sqlx::PgPool) {
        let session_id = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        super::set_session_run_status(&pool, session_id, "running").await.unwrap();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, status)
             VALUES ($1, 'assistant', 'partial answer', 'streaming')"
        ).bind(session_id).execute(&pool).await.unwrap();

        super::cleanup_session_terminated(&pool, session_id, "timeout", "r")
            .await.unwrap();

        let row: (String, String) = sqlx::query_as(
            "SELECT content, status FROM messages WHERE session_id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, "partial answer", "content preserved (not DELETE)");
        assert_eq!(row.1, "interrupted", "status flipped to interrupted");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_session_terminated_writes_timeline_event(pool: sqlx::PgPool) {
        let session_id = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        super::set_session_run_status(&pool, session_id, "running").await.unwrap();

        super::cleanup_session_terminated(&pool, session_id, "timeout", "watchdog_X")
            .await.unwrap();

        let evt: (String, serde_json::Value) = sqlx::query_as(
            "SELECT event_type, payload FROM session_timeline
             WHERE session_id = $1 ORDER BY id DESC LIMIT 1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(evt.0, "timeout");
        assert_eq!(evt.1["reason"], "watchdog_X");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_session_terminated_rolls_back_on_failure(pool: sqlx::PgPool) {
        let session_id = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        super::set_session_run_status(&pool, session_id, "running").await.unwrap();

        // Force the timeline insert (step 4) to fail by DROPping the session_timeline
        // table just before the cleanup call. The INSERT inside log_event_tx
        // will raise `relation "session_timeline" does not exist`, aborting
        // the transaction.
        //
        // Note: sqlx::test gives each test its own ephemeral DB, so dropping
        // the table affects only this test's run — safe.
        sqlx::query("DROP TABLE session_timeline").execute(&pool).await.unwrap();

        let result = super::cleanup_session_terminated(&pool, session_id, "timeout", "r")
            .await;
        assert!(result.is_err(),
            "cleanup must propagate the timeline insert error and abort the tx");

        // After the failed tx, run_status MUST be back to 'running' —
        // the atomic claim from step 1 was rolled back.
        let status: String = sqlx::query_scalar(
            "SELECT run_status FROM sessions WHERE id = $1"
        ).bind(session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(status, "running",
            "run_status must roll back to 'running' on tx failure (R-CRIT-2)");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_stale_per_agent_respects_thresholds(pool: sqlx::PgPool) {
        let s_a = super::create_new_session(&pool, "agent_a", "u", "web").await.unwrap();
        let s_b = super::create_new_session(&pool, "agent_b", "u", "web").await.unwrap();
        // Both running, both idle for 60s.
        sqlx::query("UPDATE sessions SET run_status='running',
                     activity_at = NOW() - INTERVAL '60 seconds' WHERE id = ANY($1)")
            .bind(vec![s_a, s_b]).execute(&pool).await.unwrap();

        let mut map = std::collections::HashMap::new();
        map.insert("agent_a".to_string(), 30i64);   // A times out at 30s
        map.insert("agent_b".to_string(), 600i64);  // B times out at 600s
        let stale = super::find_stale_running_sessions_per_agent(&pool, &map, 600).await.unwrap();

        let ids: Vec<uuid::Uuid> = stale.iter().map(|t| t.0).collect();
        assert!(ids.contains(&s_a), "agent_a stale (60s > 30s)");
        assert!(!ids.contains(&s_b), "agent_b not stale (60s < 600s)");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_stale_per_agent_uses_default_for_unknown(pool: sqlx::PgPool) {
        let s_x = super::create_new_session(&pool, "agent_x", "u", "web").await.unwrap();
        sqlx::query("UPDATE sessions SET run_status='running',
                     activity_at = NOW() - INTERVAL '700 seconds' WHERE id = $1")
            .bind(s_x).execute(&pool).await.unwrap();

        let map = std::collections::HashMap::new();  // x not in map
        let stale = super::find_stale_running_sessions_per_agent(&pool, &map, 600).await.unwrap();
        assert!(stale.iter().any(|t| t.0 == s_x), "fallback default (600s) applied");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn finalize_truly_stale_sessions_force_marks_interrupted(pool: sqlx::PgPool) {
        let s = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        sqlx::query("UPDATE sessions SET run_status='running',
                     activity_at = NOW() - INTERVAL '2 hours' WHERE id = $1")
            .bind(s).execute(&pool).await.unwrap();

        let n = super::finalize_truly_stale_sessions(&pool, 3600).await.unwrap();  // 1h threshold
        assert_eq!(n, 1);
        let status: String = sqlx::query_scalar(
            "SELECT run_status FROM sessions WHERE id = $1"
        ).bind(s).fetch_one(&pool).await.unwrap();
        assert_eq!(status, "interrupted");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_interrupted_sessions_uses_cleanup_helper(pool: sqlx::PgPool) {
        let s = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        super::set_session_run_status(&pool, s, "running").await.unwrap();
        sqlx::query("INSERT INTO messages (session_id, role, content, status)
                     VALUES ($1, 'assistant', 'partial', 'streaming')")
            .bind(s).execute(&pool).await.unwrap();

        super::cleanup_interrupted_sessions(&pool).await.unwrap();

        // Session marked interrupted, streaming message preserved as interrupted (not deleted).
        let session_status: String = sqlx::query_scalar(
            "SELECT run_status FROM sessions WHERE id = $1"
        ).bind(s).fetch_one(&pool).await.unwrap();
        assert_eq!(session_status, "interrupted");

        let (content, msg_status): (String, String) = sqlx::query_as(
            "SELECT content, status FROM messages WHERE session_id = $1"
        ).bind(s).fetch_one(&pool).await.unwrap();
        assert_eq!(content, "partial", "partial text preserved");
        assert_eq!(msg_status, "interrupted");

        // Timeline event written via cleanup_session_terminated.
        let event_type: String = sqlx::query_scalar(
            "SELECT event_type FROM session_timeline WHERE session_id = $1 ORDER BY id DESC LIMIT 1"
        ).bind(s).fetch_one(&pool).await.unwrap();
        assert_eq!(event_type, "interrupted");
    }

    /// R-LOOP regression: a TERMINAL session ('done'/'failed') that still
    /// carries an orphaned `status='streaming'` message must NOT cause
    /// `cleanup_interrupted_sessions` to keep returning a non-zero count
    /// forever (the startup infinite-loop that prevented the gateway from
    /// booting). The streaming row is repaired to 'interrupted' and the
    /// second call returns 0.
    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_interrupted_sessions_converges_on_terminal_streaming_leftover(
        pool: sqlx::PgPool,
    ) {
        let s = super::create_new_session(&pool, "a", "u", "web").await.unwrap();
        // Simulate the catastrophic state: session is already terminal ('done')
        // but a streaming placeholder leaked (converter killed before delete).
        super::set_session_run_status(&pool, s, "done").await.unwrap();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, status)
             VALUES ($1, 'assistant', 'leftover', 'streaming')",
        )
        .bind(s)
        .execute(&pool)
        .await
        .unwrap();

        // First sweep repairs the orphan streaming row (step 0). It returns 0
        // because the session is NOT 'running' (the only batch-counted shape).
        let first = super::cleanup_interrupted_sessions(&pool).await.unwrap();

        // The streaming row must have been cleared so it stops matching.
        let leftover: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE session_id = $1 AND status = 'streaming'",
        )
        .bind(s)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(leftover, 0, "orphan streaming row must be repaired, not left to loop");

        // Session status must NOT be clobbered back to running/interrupted —
        // a 'done' session stays 'done'.
        let status: String = sqlx::query_scalar("SELECT run_status FROM sessions WHERE id = $1")
            .bind(s)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "done", "terminal status preserved");

        // Convergence: a subsequent sweep finds nothing (loop would terminate).
        let second = super::cleanup_interrupted_sessions(&pool).await.unwrap();
        assert_eq!(second, 0, "second sweep returns 0 — startup loop converges");
        // First sweep also returned 0 (no 'running' sessions), proving the
        // batch loop in main.rs breaks immediately instead of spinning.
        assert_eq!(first, 0);
    }
}

#[cfg(test)]
mod resolve_active_dm_session_tests {
    use super::*;

    #[test]
    fn dm_scope_keys_per_channel_peer() {
        assert_eq!(
            dm_scope_keys("alice", "telegram", "per-channel-peer", None),
            ("alice", "telegram", None),
        );
    }

    #[test]
    fn dm_scope_keys_per_channel_peer_threads_chat_scope() {
        // T03 triage Point 5 regression: chat_scope must flow through
        // unmodified for the default scope, so two different chats for the
        // same user_id resolve to different lookup keys.
        assert_eq!(
            dm_scope_keys("alice", "telegram", "per-channel-peer", Some("100")),
            ("alice", "telegram", Some("100")),
        );
        assert_eq!(
            dm_scope_keys("alice", "telegram", "per-channel-peer", Some("200")),
            ("alice", "telegram", Some("200")),
        );
    }

    #[test]
    fn dm_scope_keys_shared_strips_channel_and_chat_scope() {
        // "shared"/"per-peer" deliberately want ONE session regardless of
        // platform or chat — chat_scope must be dropped to None even when
        // the caller supplied one.
        assert_eq!(dm_scope_keys("alice", "telegram", "shared", Some("100")), ("alice", "*", None));
        assert_eq!(dm_scope_keys("alice", "discord", "per-peer", Some("200")), ("alice", "*", None));
    }

    #[test]
    fn dm_scope_keys_per_chat_strips_user_and_uses_chat_scope() {
        // T03 triage Point 5 fix: "per-chat" must resolve by chat_scope, not
        // by the bare channel label (previously collapsed ALL chats on a
        // platform into one session — isolation was completely broken).
        assert_eq!(
            dm_scope_keys("alice", "telegram", "per-chat", Some("100")),
            ("*", "telegram", Some("100")),
        );
        // Two different chats under "per-chat" must carry different
        // chat_scope so they resolve to different sessions.
        let (u1, c1, s1) = dm_scope_keys("alice", "telegram", "per-chat", Some("100"));
        let (u2, c2, s2) = dm_scope_keys("bob", "telegram", "per-chat", Some("200"));
        assert_eq!((u1, c1), (u2, c2), "both collapse user to '*' and share channel");
        assert_ne!(s1, s2, "different chat_scope must remain distinguishable under per-chat");
    }

    #[test]
    fn dm_scope_keys_unknown_falls_back_to_per_channel_peer() {
        assert_eq!(
            dm_scope_keys("alice", "telegram", "garbage", Some("100")),
            ("alice", "telegram", Some("100")),
        );
    }

    /// DM with no chat concept on the platform (chat_scope = None) must
    /// degrade gracefully — never panic — and stay stable across calls.
    #[test]
    fn dm_scope_keys_no_chat_scope_degrades_gracefully() {
        let a = dm_scope_keys("alice", "whatsapp", "per-channel-peer", None);
        let b = dm_scope_keys("alice", "whatsapp", "per-channel-peer", None);
        assert_eq!(a, b);
        assert_eq!(a, ("alice", "whatsapp", None));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_returns_none_when_no_session(pool: sqlx::PgPool) {
        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_returns_recent_done_session(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "done").await.unwrap();

        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await
            .unwrap();
        assert_eq!(got, Some((sid, Some(SessionStatus::Done))));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_reuses_failed_session(pool: sqlx::PgPool) {
        // R-CONTINUITY: soft-terminal sessions within 4h ARE now reused so the
        // conversation continues instead of forking a fresh, context-less one.
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "failed").await.unwrap();

        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await
            .unwrap();
        assert_eq!(
            got,
            Some((sid, Some(SessionStatus::Failed))),
            "failed session must be reused for continuity"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_reuses_interrupted_timeout_cancelled(pool: sqlx::PgPool) {
        for (status, parsed) in [
            ("interrupted", SessionStatus::Interrupted),
            ("timeout", SessionStatus::Timeout),
            ("cancelled", SessionStatus::Cancelled),
        ] {
            let sid = create_new_session(&pool, "agent", &format!("u_{status}"), "telegram").await.unwrap();
            set_session_run_status(&pool, sid, status).await.unwrap();

            let got = resolve_active_dm_session(&pool, "agent", &format!("u_{status}"), "telegram", "per-channel-peer", None)
                .await
                .unwrap();
            assert_eq!(got, Some((sid, Some(parsed))), "{status} session must be reused");
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_returns_running_session(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "running").await.unwrap();

        let got = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await
            .unwrap();
        assert_eq!(got, Some((sid, Some(SessionStatus::Running))));
    }

    /// T03 triage Point 5 regression: a session created for chat_scope="100"
    /// must NOT be found when resolving with chat_scope="200" (different
    /// chat) — this is the core cross-chat leak fix.
    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_does_not_cross_chat_scopes(pool: sqlx::PgPool) {
        let (sid_a, _) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer", Some("100"))
            .await
            .unwrap();

        let got_same = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer", Some("100"))
            .await
            .unwrap();
        assert_eq!(got_same.map(|(id, _)| id), Some(sid_a), "same chat_scope must resolve to the same session");

        let got_other = resolve_active_dm_session(&pool, "agent", "alice", "telegram", "per-channel-peer", Some("200"))
            .await
            .unwrap();
        assert!(got_other.is_none(), "different chat_scope must NOT resolve to group A's session");
    }
}

#[cfg(test)]
mod get_or_create_with_mode_tests {
    use super::*;
    use crate::ReentryMode;

    #[sqlx::test(migrations = "../../migrations")]
    async fn fresh_session_classified_as_new(pool: sqlx::PgPool) {
        let (sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
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

        let (got_sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await
            .unwrap();
        assert_eq!(got_sid, sid);
        assert_eq!(mode, ReentryMode::NewTurnAfterDone);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn running_session_classified_as_resume(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "running").await.unwrap();

        let (got_sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await
            .unwrap();
        assert_eq!(got_sid, sid);
        assert_eq!(mode, ReentryMode::ResumeRunning);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn failed_session_reused_as_explicit_resume(pool: sqlx::PgPool) {
        // R-CONTINUITY: a failed session within 4h is reused (not forked) and
        // classified ExplicitResume so the claim allows soft-terminal→running
        // and the loop detector warms from the prior timeline.
        let old = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, old, "failed").await.unwrap();

        let (got_sid, mode) = get_or_create_session(&pool, "agent", "alice", "telegram", "per-channel-peer", None)
            .await
            .unwrap();
        assert_eq!(got_sid, old, "failed session must be reused for continuity");
        assert_eq!(mode, ReentryMode::ExplicitResume);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn interrupted_timeout_cancelled_reused_as_explicit_resume(pool: sqlx::PgPool) {
        for status in ["interrupted", "timeout", "cancelled"] {
            let old = create_new_session(&pool, "agent", &format!("u_{status}"), "telegram").await.unwrap();
            set_session_run_status(&pool, old, status).await.unwrap();

            let (got_sid, mode) = get_or_create_session(&pool, "agent", &format!("u_{status}"), "telegram", "per-channel-peer", None)
                .await
                .unwrap();
            assert_eq!(got_sid, old, "{status} session must be reused for continuity");
            assert_eq!(mode, ReentryMode::ExplicitResume, "{status} → ExplicitResume");
        }
    }

    /// T03 triage Point 5 — the headline regression test: the SAME user_id
    /// writing to two different chats (group A, group B) on the SAME
    /// platform must get TWO DIFFERENT sessions, not one collapsed session
    /// leaking group A's history into group B.
    #[sqlx::test(migrations = "../../migrations")]
    async fn different_chat_scope_same_user_yields_different_sessions(pool: sqlx::PgPool) {
        let (sid_a, mode_a) = get_or_create_session(
            &pool, "agent", "alice", "telegram", "per-channel-peer", Some("100"),
        ).await.unwrap();
        assert_eq!(mode_a, ReentryMode::NewSession);

        let (sid_b, mode_b) = get_or_create_session(
            &pool, "agent", "alice", "telegram", "per-channel-peer", Some("200"),
        ).await.unwrap();
        assert_eq!(mode_b, ReentryMode::NewSession, "group B must be a fresh session too");

        assert_ne!(sid_a, sid_b, "different chat_scope MUST yield different sessions (T03 Point 5)");

        // Calling again with chat_scope="100" must find the SAME session A,
        // not fork a third one.
        let (sid_a_again, mode_a_again) = get_or_create_session(
            &pool, "agent", "alice", "telegram", "per-channel-peer", Some("100"),
        ).await.unwrap();
        assert_eq!(sid_a_again, sid_a, "re-entering chat_scope=100 must reuse session A");
        assert_ne!(mode_a_again, ReentryMode::NewSession, "must NOT fork a third session for the same chat_scope");
    }

    /// `dm_scope = "per-chat"` (T03 triage Point 5 fix): two different users
    /// writing in the SAME chat (chat_scope) must land in the SAME
    /// group-chat session, and the same user in TWO DIFFERENT chats must
    /// land in DIFFERENT sessions. Previously "per-chat" used only the bare
    /// `channel` label, collapsing ALL chats of a platform into one row.
    #[sqlx::test(migrations = "../../migrations")]
    async fn per_chat_scope_resolves_by_chat_not_bare_channel(pool: sqlx::PgPool) {
        let (sid_alice, _) = get_or_create_session(
            &pool, "agent", "alice", "telegram", "per-chat", Some("100"),
        ).await.unwrap();
        let (sid_bob, _) = get_or_create_session(
            &pool, "agent", "bob", "telegram", "per-chat", Some("100"),
        ).await.unwrap();
        assert_eq!(sid_alice, sid_bob, "same chat_scope under per-chat must share ONE group session");

        let (sid_other_chat, _) = get_or_create_session(
            &pool, "agent", "alice", "telegram", "per-chat", Some("200"),
        ).await.unwrap();
        assert_ne!(sid_alice, sid_other_chat, "different chat_scope under per-chat must be a DIFFERENT session");
    }

    /// DM without a chat_scope (adapter has no chat concept, e.g. WhatsApp)
    /// must not panic and must produce a stable session across re-entries.
    #[sqlx::test(migrations = "../../migrations")]
    async fn no_chat_scope_degrades_to_stable_session(pool: sqlx::PgPool) {
        let (sid1, mode1) = get_or_create_session(
            &pool, "agent", "alice", "whatsapp", "per-channel-peer", None,
        ).await.unwrap();
        assert_eq!(mode1, ReentryMode::NewSession);

        let (sid2, mode2) = get_or_create_session(
            &pool, "agent", "alice", "whatsapp", "per-channel-peer", None,
        ).await.unwrap();
        assert_eq!(sid2, sid1, "no-chat-scope platform must reuse the same session on re-entry");
        assert_ne!(mode2, ReentryMode::NewSession, "second call must NOT fork a new session");
    }
}

#[cfg(test)]
mod claim_for_reentry_tests {
    use super::*;
    use crate::ReentryMode;

    #[sqlx::test(migrations = "../../migrations")]
    async fn new_session_transitions_null_to_running(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        let claimed = claim_session_for_reentry(&pool, sid, ReentryMode::NewSession).await.unwrap();
        assert!(claimed);
        let s = get_session_run_status(&pool, sid).await.unwrap();
        assert_eq!(s.as_deref(), Some("running"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn new_turn_transitions_done_to_running(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "done").await.unwrap();
        let claimed = claim_session_for_reentry(&pool, sid, ReentryMode::NewTurnAfterDone).await.unwrap();
        assert!(claimed);
        let s = get_session_run_status(&pool, sid).await.unwrap();
        assert_eq!(s.as_deref(), Some("running"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resume_running_is_idempotent(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "running").await.unwrap();
        let claimed = claim_session_for_reentry(&pool, sid, ReentryMode::ResumeRunning).await.unwrap();
        assert!(claimed);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn explicit_resume_works_from_failed(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "failed").await.unwrap();
        let claimed = claim_session_for_reentry(&pool, sid, ReentryMode::ExplicitResume).await.unwrap();
        assert!(claimed, "ExplicitResume must allow failed → running");
        let s = get_session_run_status(&pool, sid).await.unwrap();
        assert_eq!(s.as_deref(), Some("running"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn strict_mode_rejects_wrong_prior(pool: sqlx::PgPool) {
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "done").await.unwrap();
        // NewSession requires NULL prior — done is wrong.
        let claimed = claim_session_for_reentry(&pool, sid, ReentryMode::NewSession).await.unwrap();
        assert!(!claimed);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn missing_session_returns_false(pool: sqlx::PgPool) {
        let claimed = claim_session_for_reentry(&pool, Uuid::new_v4(), ReentryMode::NewTurnAfterDone).await.unwrap();
        assert!(!claimed);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn retry_recovers_after_status_flip(pool: sqlx::PgPool) {
        // Scenario: resolve saw 'done', but by the time we claim, status is 'failed'.
        let sid = create_new_session(&pool, "agent", "alice", "telegram").await.unwrap();
        set_session_run_status(&pool, sid, "failed").await.unwrap();
        // Caller naively passes NewTurnAfterDone (stale resolve result).
        let claimed = claim_session_with_retry(&pool, sid, ReentryMode::NewTurnAfterDone).await.unwrap();
        assert!(claimed, "retry with ExplicitResume must succeed");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn retry_returns_false_for_missing(pool: sqlx::PgPool) {
        let claimed = claim_session_with_retry(&pool, Uuid::new_v4(), ReentryMode::NewSession).await.unwrap();
        assert!(!claimed);
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
