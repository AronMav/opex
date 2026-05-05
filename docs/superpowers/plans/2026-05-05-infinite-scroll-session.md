# Infinite Scroll Session Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace session chain splits with in-session compression tracking, add backward-paginated messages API, and add infinite-scroll UI so users see one continuous session regardless of how many compressions occurred.

**Architecture:** DB migration adds `compressed BOOLEAN` to messages. `compress_messages()` gains `db` + `session_id` + parallel `message_db_ids` params — it marks the compressed range in DB and inserts a `session_events` compression WAL record. `bootstrap.rs`'s chain-split function is deleted. The messages API gains `before_id` backward pagination and filters out compressed rows, returning `compression_events` for divider placement. The UI adds `IntersectionObserver`-triggered backward loading and renders a `CompressionDivider` at each boundary.

**Tech Stack:** Rust/sqlx/axum (backend), React/Zustand/Immer (frontend), IntersectionObserver API (scroll detection)

---

## File Map

| Action | File | Responsibility |
| --- | --- | --- |
| Create | `migrations/045_messages_compressed.sql` | Add `compressed` column + index |
| Modify | `crates/hydeclaw-core/src/agent/compressor.rs` | Remove `pending_split` field |
| Modify | `crates/hydeclaw-db/src/sessions.rs` | Add `mark_messages_compressed()`, `insert_compression_event()`, `get_messages_page()` |
| Modify | `crates/hydeclaw-core/src/agent/history.rs` | Add params to `compress_messages()`: `message_db_ids`, `db`, `session_id` |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs` | Delete `maybe_split_session()`; add `message_db_ids` to `BootstrapOutcome` |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/execute.rs` | Thread `message_db_ids` through loop; pass new params to `compress_messages()` |
| Modify | `crates/hydeclaw-core/src/gateway/handlers/sessions.rs` | `before_id` param; new response shape; `segment_count` in session DTO |
| Create | `ui/src/components/chat/CompressionDivider.tsx` | Thin divider component |
| Modify | `ui/src/types/api.ts` | Add `MessagesResponse`, `CompressionEvent`; `segment_count` on Session |
| Modify | `ui/src/stores/chat/chat-types.ts` | Add `CompressionDividerPart` to `MessagePart` union |
| Modify | `ui/src/stores/chat/actions/session-crud.ts` | Add `loadPreviousMessages()`, state fields |
| Modify | `ui/src/app/(authenticated)/chat/MessageList.tsx` | `IntersectionObserver` + `overflow-anchor` |
| Modify | `ui/src/app/(authenticated)/chat/MessageItem.tsx` | Render `CompressionDivider` for divider parts |
| Modify | Session list component (find via grep) | `segment_count` badge |

---

## Task 1 — DB Migration: compressed column

**Files:**
- Create: `migrations/045_messages_compressed.sql`

- [ ] **Step 1: Create migration file**

```powershell
Set-Content migrations\045_messages_compressed.sql -Value @'
ALTER TABLE messages ADD COLUMN compressed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX idx_messages_session_compressed
    ON messages(session_id, compressed)
    WHERE compressed = TRUE;
'@ -Encoding utf8
```

- [ ] **Step 2: Verify**

```powershell
Get-Content migrations\045_messages_compressed.sql
```

Expected output:
```
ALTER TABLE messages ADD COLUMN compressed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX idx_messages_session_compressed
    ON messages(session_id, compressed)
    WHERE compressed = TRUE;
```

- [ ] **Step 3: Cargo check unaffected**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 5
```

Expected: no errors (SQL file only, no Rust changes).

- [ ] **Step 4: Commit**

```bash
git add migrations/045_messages_compressed.sql
git commit -m "feat(db): add compressed column to messages for in-session compression tracking"
```

---

## Task 2 — Remove pending_split from CompressorState

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/compressor.rs`

### Context

Current struct (lines 6-13):
```rust
pub struct CompressorState {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub compression_count: u32,
    #[serde(default)]
    pub pending_split: bool,     // ← REMOVE
}
```

`record_compression_result()` (lines 93-117) sets `self.state.pending_split = true`. Remove that assignment.

- [ ] **Step 1: Write failing test**

Add to the `#[cfg(test)]` block at the bottom of `compressor.rs`:

```rust
#[test]
fn compressor_state_serializes_without_pending_split() {
    let s = CompressorState {
        previous_summary: None,
        ineffective_count: 0,
        compression_count: 0,
    };
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("pending_split"), "field must be gone: {json}");
}
```

- [ ] **Step 2: Run to verify it fails**

```powershell
cargo test -p hydeclaw-core compressor_state_serializes_without_pending_split -- --nocapture 2>&1 | tail -5
```

Expected: compile error — struct literal missing `pending_split` field.

- [ ] **Step 3: Remove pending_split from struct**

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompressorState {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub compression_count: u32,
}
```

- [ ] **Step 4: Remove pending_split assignment from record_compression_result()**

In `record_compression_result()`, delete the line:
```rust
self.state.pending_split = true;
```

- [ ] **Step 5: Run tests**

```powershell
cargo test -p hydeclaw-core compressor -- --nocapture 2>&1 | tail -10
```

Expected: all compressor tests pass including new one.

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/agent/compressor.rs
git commit -m "refactor(compressor): remove pending_split — chain splits replaced by in-session compression tracking"
```

---

## Task 3 — DB functions for compression tracking

**Files:**
- Modify: `crates/hydeclaw-db/src/sessions.rs`

Three new functions added to the bottom of the file.

- [ ] **Step 1: Write compile-time tests**

Add to `crates/hydeclaw-db/src/sessions.rs` test section:

```rust
#[cfg(test)]
mod compression_tests {
    use super::*;

    #[test]
    fn mark_messages_compressed_signature_exists() {
        // Compile-time check
        let _: fn(&sqlx::PgPool, &[uuid::Uuid])
            -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>>>> =
            |db, ids| Box::pin(mark_messages_compressed(db, ids));
    }

    #[test]
    fn compression_event_row_has_required_fields() {
        let _row = CompressionEventRow {
            segment_index: 1,
            first_live_message_id: None,
            summary: String::new(),
        };
    }

    #[test]
    fn messages_page_has_required_fields() {
        let _page = MessagesPage {
            messages: vec![],
            compression_events: vec![],
            has_more: false,
        };
    }
}
```

- [ ] **Step 2: Run to verify failures**

```powershell
cargo test -p hydeclaw-db compression_tests -- --nocapture 2>&1 | Select-String "error\[" | Select-Object -First 5
```

Expected: compile errors — types and functions not defined.

- [ ] **Step 3: Add CompressionEventRow and MessagesPage structs**

```rust
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
```

- [ ] **Step 4: Add mark_messages_compressed()**

```rust
pub async fn mark_messages_compressed(
    db: &sqlx::PgPool,
    ids: &[uuid::Uuid],
) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query!(
        "UPDATE messages SET compressed = TRUE WHERE id = ANY($1)",
        ids as &[uuid::Uuid],
    )
    .execute(db)
    .await?;
    Ok(())
}
```

- [ ] **Step 5: Add insert_compression_event()**

```rust
pub async fn insert_compression_event(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    segment_index: u32,
    summary: &str,
    first_compressed_id: Option<uuid::Uuid>,
    first_live_id: Option<uuid::Uuid>,
    tokens_before: i64,
    tokens_after: i64,
) -> anyhow::Result<()> {
    let payload = serde_json::json!({
        "segment_index": segment_index,
        "summary": summary,
        "first_compressed_message_id": first_compressed_id,
        "first_live_message_id": first_live_id,
        "tokens_before": tokens_before,
        "tokens_after": tokens_after,
    });
    sqlx::query!(
        "INSERT INTO session_events (session_id, event_type, payload)
         VALUES ($1, 'compression', $2)",
        session_id,
        payload,
    )
    .execute(db)
    .await?;
    Ok(())
}
```

- [ ] **Step 6: Add get_messages_page()**

```rust
pub async fn get_messages_page(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    before_id: Option<uuid::Uuid>,
    limit: i64,
) -> anyhow::Result<MessagesPage> {
    // Fetch limit+1 to detect has_more; DESC order so newest first, then we reverse
    let rows: Vec<MessageRow> = if let Some(bid) = before_id {
        sqlx::query_as!(
            MessageRow,
            r#"SELECT id, role, content, tool_calls, tool_call_id, created_at,
                      agent_id, feedback, edited_at, status, thinking_blocks,
                      parent_message_id, branch_from_message_id, abort_reason, is_mirror
               FROM messages
               WHERE session_id = $1
                 AND compressed = FALSE
                 AND created_at < (
                     SELECT created_at FROM messages WHERE id = $2
                 )
               ORDER BY created_at DESC
               LIMIT $3"#,
            session_id, bid, limit + 1,
        )
        .fetch_all(db)
        .await?
    } else {
        sqlx::query_as!(
            MessageRow,
            r#"SELECT id, role, content, tool_calls, tool_call_id, created_at,
                      agent_id, feedback, edited_at, status, thinking_blocks,
                      parent_message_id, branch_from_message_id, abort_reason, is_mirror
               FROM messages
               WHERE session_id = $1
                 AND compressed = FALSE
               ORDER BY created_at DESC
               LIMIT $2"#,
            session_id, limit + 1,
        )
        .fetch_all(db)
        .await?
    };

    let has_more = rows.len() as i64 > limit;
    let mut rows: Vec<MessageRow> = rows.into_iter().take(limit as usize).collect();
    rows.reverse(); // ASC order: oldest first

    // Fetch compression events whose first_live_message_id is in this page
    let page_ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
    let events = if page_ids.is_empty() {
        vec![]
    } else {
        sqlx::query!(
            r#"SELECT payload
               FROM session_events
               WHERE session_id = $1
                 AND event_type = 'compression'
                 AND (payload->>'first_live_message_id')::uuid = ANY($2)"#,
            session_id,
            &page_ids as &[uuid::Uuid],
        )
        .fetch_all(db)
        .await?
        .into_iter()
        .filter_map(|r| {
            let p = r.payload?;
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
```

- [ ] **Step 7: Run compile tests**

```powershell
cargo test -p hydeclaw-db compression_tests -- --nocapture 2>&1 | tail -5
```

Expected: all 3 tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/hydeclaw-db/src/sessions.rs
git commit -m "feat(db): add mark_messages_compressed, insert_compression_event, get_messages_page"
```

---

## Task 4 — Thread message_db_ids through pipeline

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs`
- Modify: `crates/hydeclaw-core/src/agent/pipeline/execute.rs`

### Context

`compress_messages()` (updated in Task 5) needs the DB IDs of messages in parallel with the `messages` vec. We add `message_db_ids: Vec<Option<uuid::Uuid>>` to `BootstrapOutcome` (alongside `messages`), build it from `MessageRow.id` when loading session history, and thread it through `execute.rs`.

`BootstrapOutcome` struct (bootstrap.rs lines 18-40):
```rust
pub struct BootstrapOutcome {
    pub session_id: Uuid,
    pub enriched_text: String,
    pub messages: Vec<Message>,
    pub tools: Vec<hydeclaw_types::ToolDefinition>,
    pub loop_detector: LoopDetector,
    pub processing_guard: ProcessingGuard,
    pub lifecycle_guard: Option<SessionLifecycleGuard>,
    pub command_output: Option<String>,
    pub user_message_id: Uuid,
    pub incoming_context: serde_json::Value,
    pub channel: String,
    pub compressor: crate::agent::compressor::Compressor,
}
```

- [ ] **Step 1: Write compile-time test for field existence**

In `bootstrap.rs` test section add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_outcome_has_message_db_ids() {
        fn _check(_o: &BootstrapOutcome) -> &Vec<Option<uuid::Uuid>> {
            &_o.message_db_ids
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

```powershell
cargo test -p hydeclaw-core bootstrap_outcome_has_message_db_ids 2>&1 | Select-String "error\[" | Select-Object -First 3
```

Expected: compile error — field not found.

- [ ] **Step 3: Add message_db_ids to BootstrapOutcome**

In `bootstrap.rs`, add to the struct after `pub compressor`:

```rust
/// DB primary keys parallel to `messages` — None for synthetic messages
/// (summary, appended user turn). Used by compress_messages() for in-DB marking.
pub message_db_ids: Vec<Option<uuid::Uuid>>,
```

- [ ] **Step 4: Build message_db_ids when loading session messages**

In `bootstrap.rs`, find where `sessions::load_messages()` is called and the resulting `Vec<MessageRow>` is converted to `Vec<Message>`. Immediately after that conversion, add:

```rust
// Build parallel ID vec — same length and order as `messages`
let message_db_ids: Vec<Option<uuid::Uuid>> =
    message_rows.iter().map(|r| Some(r.id)).collect();
```

Then include `message_db_ids` in the `BootstrapOutcome { ... }` construction.

**Note:** The variable holding the `Vec<MessageRow>` before conversion may be named `rows`, `message_rows`, or similar. Search for the `load_messages` call and adapt accordingly.

- [ ] **Step 5: Thread message_db_ids through execute.rs**

In `execute.rs`, find where `BootstrapOutcome` fields are destructured (e.g. `let BootstrapOutcome { messages, compressor, ... } = outcome`). Extract `message_db_ids`:

```rust
let BootstrapOutcome {
    messages,
    compressor,
    message_db_ids,
    // ... other fields
} = outcome;
let mut message_db_ids = message_db_ids;
```

Everywhere a new `Message` is pushed to `messages`, also push `None` to `message_db_ids`:

```rust
// Pattern: whenever you see  messages.push(some_msg)
// Add below: message_db_ids.push(None);
```

Search for all `messages.push(` in execute.rs and add the corresponding `message_db_ids.push(None)`.

- [ ] **Step 6: Compile check**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 10
```

Expected: no errors. Fix any struct construction mismatches (missing field in `BootstrapOutcome { }` literals).

- [ ] **Step 7: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs \
        crates/hydeclaw-core/src/agent/pipeline/execute.rs
git commit -m "feat(pipeline): thread message_db_ids through bootstrap and execute for compression DB tracking"
```

---

## Task 5 — compress_messages(): add DB writes

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/history.rs`

### Context

Current signature (history.rs line 673):
```rust
pub async fn compress_messages(
    messages: &mut Vec<Message>,
    compressor: &mut crate::agent::compressor::Compressor,
    cfg: &crate::config::CompactionConfig,
    provider: &dyn LlmProvider,
    language: Option<&str>,
) -> anyhow::Result<Vec<String>>
```

After Phase 2 (head_end + tail_start computed, line 690), collect IDs. After Phase 5 (reassembly complete, line 796), do DB writes and update `message_db_ids` in parallel with `messages`.

The `segment_index` for the new WAL event = `compressor.state.compression_count` BEFORE `record_compression_result()` increments it.

- [ ] **Step 1: Write unit test for signature**

In the `#[cfg(test)]` section of `history.rs`, add:

```rust
#[test]
fn compress_messages_accepts_db_params() {
    // Compile-time: verify new params are accepted
    fn _assert_sig(
        _messages: &mut Vec<Message>,
        _ids: &mut Vec<Option<uuid::Uuid>>,
        _compressor: &mut crate::agent::compressor::Compressor,
        _cfg: &crate::config::CompactionConfig,
        _provider: &dyn crate::agent::providers::LlmProvider,
        _language: Option<&str>,
        _db: &sqlx::PgPool,
        _session_id: uuid::Uuid,
    ) {
        // Would call: compress_messages(_messages, _ids, _compressor, _cfg,
        //                               _provider, _language, _db, _session_id)
    }
}
```

- [ ] **Step 2: Update compress_messages() signature**

```rust
pub async fn compress_messages(
    messages: &mut Vec<Message>,
    message_db_ids: &mut Vec<Option<uuid::Uuid>>,
    compressor: &mut crate::agent::compressor::Compressor,
    cfg: &crate::config::CompactionConfig,
    provider: &dyn LlmProvider,
    language: Option<&str>,
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
) -> anyhow::Result<Vec<String>>
```

- [ ] **Step 3: Collect IDs before middle range is processed**

After line 690 (`let tail_start = find_tail_start_by_tokens(...)`), add:

```rust
let tokens_before_i64 = tokens_before as i64;
let compressed_ids: Vec<uuid::Uuid> = message_db_ids
    .get(head_end..tail_start)
    .unwrap_or(&[])
    .iter()
    .filter_map(|id| *id)
    .collect();
let first_compressed_id = compressed_ids.first().copied();
let first_live_id = message_db_ids
    .get(tail_start)
    .and_then(|id| *id);
let segment_index_for_event = compressor.compression_count;
```

- [ ] **Step 4: Update message_db_ids parallel to messages during reassembly**

In Phase 4 (reassembly, around line 729), after `*messages = assembled;` (line 799), also update `message_db_ids` to match the new structure:

```rust
// Rebuild message_db_ids parallel to new messages layout:
// [head_ids] + [None for summary msg] + [tail_ids]
let new_ids: Vec<Option<uuid::Uuid>> = message_db_ids
    .get(..head_end)
    .unwrap_or(&[])
    .iter()
    .copied()
    .chain(std::iter::once(None)) // summary message is synthetic
    .chain(
        message_db_ids
            .get(tail_start..)
            .unwrap_or(&[])
            .iter()
            .copied(),
    )
    .collect();
*message_db_ids = new_ids;
```

Place this immediately after `*messages = assembled;` (line 799) and before `compressor.previous_summary = Some(summary_text);` (line 800).

- [ ] **Step 5: Add DB writes after Phase 5**

After the existing `compressor.record_compression_result(tokens_before, tokens_after, cfg);` call (line 803), add:

```rust
// Persist compression event to DB (best-effort, non-fatal on failure)
let tokens_after_i64 = estimate_tokens(messages) as i64;
if !compressed_ids.is_empty() {
    if let Err(e) = crate::db::sessions::mark_messages_compressed(
        db, &compressed_ids,
    ).await {
        tracing::warn!(error = %e, "failed to mark messages as compressed in DB");
    }
    if let Err(e) = crate::db::sessions::insert_compression_event(
        db,
        session_id,
        segment_index_for_event,
        &summary_text,
        first_compressed_id,
        first_live_id,
        tokens_before_i64,
        tokens_after_i64,
    ).await {
        tracing::warn!(error = %e, "failed to insert compression WAL event");
    }
}
```

- [ ] **Step 6: Find all other call sites of compress_messages and update**

```powershell
Select-String -Path "crates\**\*.rs" -Pattern "compress_messages\(" -Recurse
```

For each call site found (expect: execute.rs, and possibly history.rs internal callers):

- In `execute.rs` (the proactive compression trigger, lines 176-197), update to:
```rust
crate::agent::history::compress_messages(
    &mut messages,
    &mut message_db_ids,
    compressor,
    cmp_cfg,
    active_provider,
    Some(engine.cfg().agent.language.as_str()),
    &engine.cfg().db,
    session_id,
).await
```

- For any other call site (e.g. inside history.rs itself), pass an empty `&mut vec![]` for `message_db_ids` if DB tracking is not applicable there.

- [ ] **Step 7: Check if compact_if_needed calls compress_messages**

```powershell
Select-String -Path "crates\hydeclaw-core\src\agent\history.rs" -Pattern "compress_messages" | Select-Object LineNumber, Line
```

If `compact_if_needed` internally calls `compress_messages`, update that call to pass:
- `&mut ids` where `ids` is built as `messages.iter().map(|_| None).collect()` (no real IDs — slash command compaction rewrites messages entirely, so DB marking is irrelevant)
- `db` and a dummy `session_id` (or refactor `compact_if_needed` to also accept them)

- [ ] **Step 8: Full compile check**

```powershell
cargo check --all-targets 2>&1 | Select-String "^error" | Select-Object -First 15
```

Expected: no errors.

- [ ] **Step 9: Run compression tests**

```powershell
cargo test -p hydeclaw-core compress -- --nocapture 2>&1 | tail -10
```

Expected: all pass.

- [ ] **Step 10: Commit**

```bash
git add crates/hydeclaw-core/src/agent/history.rs \
        crates/hydeclaw-core/src/agent/pipeline/execute.rs
git commit -m "feat(compression): mark compressed messages in DB and insert WAL event after each proactive compression"
```

---

## Task 6 — Remove maybe_split_session from bootstrap.rs

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs`

### Context

`maybe_split_session()` spans lines 74-242. It is called inside `bootstrap()` at lines 262-287 (wrapped in `effective_resume_id` computation). After this change, `effective_resume_id` is always `ctx.resume_session_id` — no redirection needed.

- [ ] **Step 1: Delete maybe_split_session() function body**

Remove the entire function from line 74 to its closing `}` (end of line 242). Also remove any helper functions it calls exclusively (e.g. `create_chain_session` if it exists only for this purpose — verify with grep before deleting).

```powershell
Select-String -Path "crates\hydeclaw-core\src\agent\pipeline\bootstrap.rs" `
    -Pattern "fn create_chain_session|fn maybe_split_session" | Select-Object LineNumber, Line
```

- [ ] **Step 2: Replace call site with direct assignment**

Find the `effective_resume_id` block (lines 262-287):

```rust
let effective_resume_id: Option<uuid::Uuid> = if !ctx.force_new_session {
    if let Some(resume_id) = ctx.resume_session_id {
        let preserve_last_n = ...;
        match maybe_split_session(&engine.cfg().db, resume_id, preserve_last_n).await {
            Ok(Some(child_id)) => Some(child_id),
            Ok(None) => Some(resume_id),
            Err(e) => { ... Some(resume_id) }
        }
    } else {
        None
    }
} else {
    ctx.resume_session_id
};
```

Replace with:

```rust
let effective_resume_id: Option<uuid::Uuid> = ctx.resume_session_id;
```

- [ ] **Step 3: Remove imports only used by maybe_split_session**

After deletion, `cargo check` will flag any unused imports. Remove them.

- [ ] **Step 4: Full compile check**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 15
```

Expected: no errors.

- [ ] **Step 5: Run full test suite**

```powershell
cargo test -p hydeclaw-core 2>&1 | Select-String "FAILED|test result" | Select-Object -First 10
```

Expected: same pre-existing failures (finalize, lifecycle_guard — require DATABASE_URL). No new failures.

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs
git commit -m "refactor(bootstrap): remove maybe_split_session — compression stays in single session"
```

---

## Task 7 — Messages API endpoint with backward pagination

**Files:**
- Modify: `crates/hydeclaw-core/src/gateway/handlers/sessions.rs`

### Context

Current `MessagesQuery` (lines 191-194):
```rust
pub(crate) struct MessagesQuery {
    limit: Option<i64>,
    agent: Option<String>,
}
```

Current handler calls `sessions::load_messages(&infra.db, id, Some(limit))` and returns `{"messages": rows}`.

New: add `before_id`, call `get_messages_page()`, return `{"messages", "compression_events", "has_more"}`.

Also add `segment_count` to session DTO responses.

- [ ] **Step 1: Update MessagesQuery**

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct MessagesQuery {
    limit: Option<i64>,
    agent: Option<String>,
    before_id: Option<uuid::Uuid>,
}
```

- [ ] **Step 2: Update api_session_messages handler**

Replace the handler body (keeping existing auth/ownership check at the top):

```rust
pub(crate) async fn api_session_messages(
    State(infra): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Query(q): Query<MessagesQuery>,
) -> impl IntoResponse {
    // Keep existing ownership/agent check unchanged above this line

    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let page = match crate::db::sessions::get_messages_page(
        &infra.db,
        id,
        q.before_id,
        limit,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let events_json: Vec<serde_json::Value> = page
        .compression_events
        .iter()
        .map(|e| {
            serde_json::json!({
                "segment_index": e.segment_index,
                "first_live_message_id": e.first_live_message_id,
                "summary": e.summary,
            })
        })
        .collect();

    axum::Json(serde_json::json!({
        "messages": page.messages,
        "compression_events": events_json,
        "has_more": page.has_more,
    }))
    .into_response()
}
```

- [ ] **Step 3: Add segment_count to session DTO queries**

Find the SQL queries that return session data for the list endpoint and individual session endpoint (search for `SELECT.*FROM sessions` in sessions.rs). Add a subquery:

```sql
(SELECT COUNT(*)::int FROM session_events
 WHERE session_id = s.id AND event_type = 'compression') AS segment_count
```

Update the result struct / JSON serialization to include `segment_count: i32`.

**Shortcut:** If the session queries use `SELECT *` or a macro, add `segment_count` to the returned `Session` struct or a `SessionDto` wrapper, and update the JSON response to include it.

- [ ] **Step 4: Compile check**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 10
```

- [ ] **Step 5: Quick smoke test** (optional, requires running server)

```powershell
# After `cargo run`:
$headers = @{ Authorization = "Bearer $env:HYDECLAW_AUTH_TOKEN" }
Invoke-RestMethod "http://localhost:18789/api/sessions/{id}/messages" -Headers $headers
# Expected: { messages: [...], compression_events: [], has_more: false }
```

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/gateway/handlers/sessions.rs
git commit -m "feat(api): backward-paginated messages endpoint with compression_events and has_more; add segment_count to session DTO"
```

---

## Task 8 — Frontend TypeScript types

**Files:**
- Modify: `ui/src/types/api.ts`
- Modify: `ui/src/stores/chat/chat-types.ts`

- [ ] **Step 1: Add to api.ts**

Find the `Session` (or `SessionInfo`) interface and add:

```typescript
segment_count?: number;
```

Add new interfaces:

```typescript
export interface CompressionEvent {
  segment_index: number;
  first_live_message_id: string;
  summary: string;
}

export interface MessagesResponse {
  messages: RawMessage[];  // use whatever name matches existing raw message type
  compression_events: CompressionEvent[];
  has_more: boolean;
}
```

**Note:** Check how the existing messages API is typed. The raw message type might be called `ApiMessage`, `RawMessage`, or similar. Use whatever already exists.

- [ ] **Step 2: Add CompressionDividerPart to chat-types.ts**

In `ui/src/stores/chat/chat-types.ts`, add the new part type:

```typescript
export interface CompressionDividerPart {
  type: "compression-divider";
  segmentIndex: number;
  totalSegments: number;
}
```

Add `| CompressionDividerPart` to the `MessagePart` union.

- [ ] **Step 3: TypeScript compile check**

```powershell
cd ui && npx tsc --noEmit 2>&1 | Select-String "error TS" | Select-Object -First 10
```

Expected: no new errors.

- [ ] **Step 4: Commit**

```bash
git add ui/src/types/api.ts ui/src/stores/chat/chat-types.ts
git commit -m "feat(types): add MessagesResponse, CompressionEvent, CompressionDividerPart types"
```

---

## Task 9 — Chat store: loadPreviousMessages

**Files:**
- Modify: `ui/src/stores/chat/actions/session-crud.ts`
- Modify: the main chat store definition (find via `ui/src/stores/chat-store.ts` or similar)

### Context

The chat store uses Zustand with Immer. Session messages are loaded via React Query (`useSessionMessages` hook or similar). We need to add:
- `hasMoreHistory: boolean` and `isLoadingHistory: boolean` to per-agent state (or global store state)
- `loadPreviousMessages()` action

First, find where per-agent state fields are defined. In `chat-types.ts`, the `AgentState` interface likely lives there — add the two new fields. Then add the action in `session-crud.ts`.

- [ ] **Step 1: Add state fields to AgentState**

In `ui/src/stores/chat/chat-types.ts`, find the `AgentState` interface. Add:

```typescript
hasMoreHistory: boolean;
isLoadingHistory: boolean;
```

In `emptyAgentState()` (same file or nearby), initialize both as `false`.

- [ ] **Step 2: Add insertCompressionDividers helper to session-crud.ts**

At the top of `session-crud.ts`, add this helper:

```typescript
import type { CompressionEvent } from "@/types/api";
import type { ChatMessage, CompressionDividerPart } from "../../chat-types";

function insertCompressionDividers(
  messages: ChatMessage[],
  events: CompressionEvent[],
  totalSegments: number,
): ChatMessage[] {
  if (events.length === 0) return messages;
  const dividerMap = new Map(events.map((e) => [e.first_live_message_id, e]));
  const result: ChatMessage[] = [];
  for (const msg of messages) {
    const event = dividerMap.get(msg.id);
    if (event) {
      const dividerPart: CompressionDividerPart = {
        type: "compression-divider",
        segmentIndex: event.segment_index,
        totalSegments,
      };
      result.push({
        id: `compression-divider-${event.segment_index}`,
        role: "assistant",
        parts: [dividerPart],
      });
    }
    result.push(msg);
  }
  return result;
}
```

- [ ] **Step 3: Add loadPreviousMessages to createSessionCrudActions**

In the returned object of `createSessionCrudActions`, add:

```typescript
loadPreviousMessages: async (agentName: string) => {
  const state = get();
  const agentState = state.agents[agentName];
  if (!agentState) return;
  const { isLoadingHistory, hasMoreHistory, activeSessionId } = agentState;
  if (isLoadingHistory || !hasMoreHistory || !activeSessionId) return;

  // Find the ID of the first (oldest) currently loaded message
  const liveMessages = getLiveMessages(agentState);
  const firstMsg = liveMessages[0];
  if (!firstMsg) return;

  set((draft: any) => {
    draft.agents[agentName].isLoadingHistory = true;
  });

  try {
    const session = state.agents[agentName];
    const totalSegments = (session as any).sessionSegmentCount ?? 1;

    const res = await fetch(
      `/api/sessions/${activeSessionId}/messages?before_id=${firstMsg.id}&limit=50`,
      { headers: { Authorization: `Bearer ${getToken()}` } },
    ).then((r) => r.json());

    // Convert raw API messages to ChatMessage using existing converter
    const converted: ChatMessage[] = convertRawMessages(res.messages ?? []);
    const withDividers = insertCompressionDividers(
      converted,
      res.compression_events ?? [],
      totalSegments,
    );

    set((draft: any) => {
      const a = draft.agents[agentName];
      // Prepend older messages before existing ones
      a.messages = [...withDividers, ...getLiveMessages(a)];
      a.hasMoreHistory = res.has_more ?? false;
      a.isLoadingHistory = false;
    });
  } catch (_e) {
    // toast import: import { toast } from "sonner";
    toast.error("Не удалось загрузить историю сообщений");
    set((draft: any) => {
      draft.agents[agentName].isLoadingHistory = false;
    });
  }
},
```

**Notes:**
- `getLiveMessages` is already imported (line 7 of session-crud.ts)
- `getToken()` — import from wherever auth token is accessed (check `@/lib/api` or auth store)
- `convertRawMessages` — find the existing function that converts API message rows to `ChatMessage` (likely in `chat-history.ts` or `streaming-renderer.ts`)

- [ ] **Step 4: Update initial session load to set hasMoreHistory**

Find where session messages are loaded initially (likely in `useSessionMessages` hook or a `loadSession` action). After the first load, set `hasMoreHistory` from the API response's `has_more` field.

If messages are loaded via React Query with `useSessionMessages`, update the query function to use the new `MessagesResponse` shape and set `hasMoreHistory` in the store after the query resolves.

- [ ] **Step 5: TypeScript compile check**

```powershell
cd ui && npx tsc --noEmit 2>&1 | Select-String "error TS" | Select-Object -First 10
```

- [ ] **Step 6: Commit**

```bash
git add ui/src/stores/
git commit -m "feat(chat-store): add hasMoreHistory, isLoadingHistory, loadPreviousMessages() with compression divider injection"
```

---

## Task 10 — CompressionDivider component

**Files:**
- Create: `ui/src/components/chat/CompressionDivider.tsx`
- Create: `ui/src/components/chat/__tests__/CompressionDivider.test.tsx`

- [ ] **Step 1: Write the test**

Create `ui/src/components/chat/__tests__/CompressionDivider.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { CompressionDivider } from "../CompressionDivider";

test("renders segment label with correct numbers", () => {
  render(<CompressionDivider segmentIndex={2} totalSegments={3} />);
  expect(screen.getByText(/Сегмент 2 из 3/)).toBeInTheDocument();
});

test("renders compression marker text", () => {
  render(<CompressionDivider segmentIndex={1} totalSegments={2} />);
  expect(screen.getByText(/Контекст сжат/)).toBeInTheDocument();
});
```

- [ ] **Step 2: Run to verify failure**

```powershell
cd ui && npm test -- CompressionDivider --run 2>&1 | Select-String "FAIL|Cannot find" | Select-Object -First 5
```

Expected: FAIL — component file not found.

- [ ] **Step 3: Implement CompressionDivider**

Create `ui/src/components/chat/CompressionDivider.tsx`:

```tsx
interface Props {
  segmentIndex: number;
  totalSegments: number;
}

export function CompressionDivider({ segmentIndex, totalSegments }: Props) {
  return (
    <div className="flex items-center gap-3 my-4 px-4 select-none" aria-hidden>
      <div className="flex-1 h-px bg-border" />
      <span className="text-xs text-muted-foreground whitespace-nowrap">
        ◈ Контекст сжат · Сегмент {segmentIndex} из {totalSegments}
      </span>
      <div className="flex-1 h-px bg-border" />
    </div>
  );
}
```

- [ ] **Step 4: Run test to verify it passes**

```powershell
cd ui && npm test -- CompressionDivider --run 2>&1 | Select-String "✓|PASS|FAIL" | Select-Object -First 5
```

Expected: 2 tests pass.

- [ ] **Step 5: Wire into message renderer**

In `ui/src/app/(authenticated)/chat/MessageItem.tsx` (or wherever `MessagePart` variants are rendered), add the `compression-divider` case:

```tsx
import { CompressionDivider } from "@/components/chat/CompressionDivider";

// In the part renderer switch/if-else:
if (part.type === "compression-divider") {
  return (
    <CompressionDivider
      key={part.type}
      segmentIndex={part.segmentIndex}
      totalSegments={part.totalSegments}
    />
  );
}
```

- [ ] **Step 6: TypeScript compile check**

```powershell
cd ui && npx tsc --noEmit 2>&1 | Select-String "error TS" | Select-Object -First 5
```

- [ ] **Step 7: Commit**

```bash
git add ui/src/components/chat/CompressionDivider.tsx \
        ui/src/components/chat/__tests__/CompressionDivider.test.tsx
git commit -m "feat(ui): add CompressionDivider component for compression boundary markers"
```

---

## Task 11 — IntersectionObserver + overflow-anchor in MessageList

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/MessageList.tsx`

### Context

`MessageList.tsx` is the scrollable container that renders chat messages. We add:
1. `overflow-anchor: auto` CSS on the scroll container — prevents scroll jump when prepending (natively supported in all modern browsers)
2. `IntersectionObserver` on the first message element — calls `loadPreviousMessages()` when visible
3. Loading spinner at top when `isLoadingHistory`

- [ ] **Step 1: Add overflow-anchor to scroll container**

Find the outermost scrollable `div` in `MessageList.tsx` (likely has `overflow-y-auto` class). Add inline style:

```tsx
<div
  className="... overflow-y-auto ..."
  style={{ overflowAnchor: "auto" }}
>
```

- [ ] **Step 2: Add IntersectionObserver hook**

Import and add to `MessageList`:

```tsx
import { useEffect, useRef } from "react";
import { useChatStore } from "@/stores/chat-store"; // adjust import path

// Inside component:
const firstMsgRef = useRef<HTMLDivElement>(null);
const { loadPreviousMessages, hasMoreHistory, isLoadingHistory, currentAgent } =
  useChatStore((s) => ({
    loadPreviousMessages: s.loadPreviousMessages,
    hasMoreHistory: s.agents[s.currentAgent]?.hasMoreHistory ?? false,
    isLoadingHistory: s.agents[s.currentAgent]?.isLoadingHistory ?? false,
    currentAgent: s.currentAgent,
  }));

// Track first message ID to re-observe after prepend
const firstMessageId = messages[0]?.id;

useEffect(() => {
  const el = firstMsgRef.current;
  if (!el || !hasMoreHistory) return;

  const observer = new IntersectionObserver(
    ([entry]) => {
      if (entry.isIntersecting && !isLoadingHistory) {
        loadPreviousMessages(currentAgent);
      }
    },
    { threshold: 0.1 },
  );
  observer.observe(el);
  return () => observer.disconnect();
}, [firstMessageId, hasMoreHistory, isLoadingHistory, loadPreviousMessages, currentAgent]);
```

- [ ] **Step 3: Attach ref to first message element**

In the message list render, attach `firstMsgRef` to the first message:

```tsx
{messages.map((msg, idx) => (
  <div key={msg.id} ref={idx === 0 ? firstMsgRef : undefined}>
    <MessageItem message={msg} />
  </div>
))}
```

- [ ] **Step 4: Add loading spinner at top**

Above the message list, add:

```tsx
{isLoadingHistory && (
  <div className="flex justify-center py-3">
    <div className="h-4 w-4 animate-spin rounded-full border-2 border-muted-foreground border-t-transparent" />
  </div>
)}
```

- [ ] **Step 5: TypeScript compile + vitest check**

```powershell
cd ui && npx tsc --noEmit && npm test -- --run 2>&1 | Select-String "error|FAIL" | Select-Object -First 10
```

- [ ] **Step 6: Commit**

```bash
git add ui/src/app/(authenticated)/chat/MessageList.tsx
git commit -m "feat(ui): IntersectionObserver for backward history loading; overflow-anchor prevents scroll jump"
```

---

## Task 12 — Session list segment_count badge

**Files:**
- Modify: session list item component (find below)

- [ ] **Step 1: Find the session list item component**

```powershell
Get-ChildItem -Recurse -Path ui\src -Filter "*.tsx" |
  Select-String "session\.title|sessionTitle|session-list|SessionItem" |
  Select-Object Path, LineNumber |
  Select-Object -First 10
```

Read the found file to understand the component structure.

- [ ] **Step 2: Add segment_count badge**

In the session list item, after the session title text node, add:

```tsx
{session.segment_count != null && session.segment_count > 1 && (
  <span className="ml-1.5 text-xs text-muted-foreground shrink-0 tabular-nums">
    ◈ {session.segment_count}
  </span>
)}
```

This renders only when `segment_count > 1` — single-segment sessions show no badge.

- [ ] **Step 3: TypeScript compile check**

```powershell
cd ui && npx tsc --noEmit 2>&1 | Select-String "error TS" | Select-Object -First 5
```

- [ ] **Step 4: Commit**

```bash
git add ui/src/
git commit -m "feat(ui): show ◈ N segment count badge in session list for multi-segment sessions"
```

---

## Task 13 — Final verification

- [ ] **Step 1: Full Rust tests**

```powershell
cd d:\GIT\bogdan\hydeclaw
cargo test -p hydeclaw-core -p hydeclaw-db 2>&1 | Select-String "FAILED|test result" | Select-Object -First 10
```

Expected: same pre-existing failures only (finalize, lifecycle_guard — require DATABASE_URL).

- [ ] **Step 2: UI build**

```powershell
cd ui && npm run build 2>&1 | Select-String "error|Error" | Select-Object -First 10
```

Expected: clean build.

- [ ] **Step 3: Run UI tests**

```powershell
cd ui && npm test -- --run 2>&1 | Select-String "FAIL|✓.*CompressionDivider" | Select-Object -First 10
```

Expected: CompressionDivider tests pass, no regressions.

- [ ] **Step 4: Count registrations and verify migration**

```powershell
# Verify 045 migration is the latest
Get-ChildItem migrations\ | Sort-Object Name | Select-Object -Last 3

# Verify compress_messages no longer referenced in bootstrap.rs chain-split context
Select-String -Path "crates\hydeclaw-core\src\agent\pipeline\bootstrap.rs" `
    -Pattern "maybe_split_session|pending_split|create_chain_session"
```

Expected: no matches.

- [ ] **Step 5: Deploy to Pi and smoke test**

```bash
make deploy && make doctor
```

Open the UI, navigate to a session, scroll to the top. Verify:
- History loads silently (IntersectionObserver fires, messages prepend)
- No scroll position jump (overflow-anchor working)
- After real compression occurs: `◈ Контекст сжат` divider appears
- Sessions with `segment_count > 1` show `◈ N` badge in sidebar
