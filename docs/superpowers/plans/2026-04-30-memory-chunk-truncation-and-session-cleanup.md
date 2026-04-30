# Memory Chunk Truncation + Session Streaming Cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent context overflow from huge memory tool results (Excalidraw files) and clean up orphaned streaming messages when a session is resumed after a crash.

**Architecture:** Two independent fixes. Fix A adds a per-chunk character cap in `memory.rs` with special-casing for binary Excalidraw content — no schema changes, no new files. Fix B adds one function in `sessions.rs` and one call in `bootstrap.rs` so any streaming message left by a crashed run is marked `'interrupted'` before the next run loads context.

**Tech Stack:** Rust 2024 edition, sqlx 0.8, tokio, cargo test --lib

---

## File Map

| File | Change |
|------|--------|
| `crates/hydeclaw-core/src/agent/pipeline/memory.rs` | Add `MEMORY_CHUNK_MAX_CHARS`, `truncate_chunk_content()`, apply in search + get handlers |
| `crates/hydeclaw-db/src/sessions.rs` | Add `cleanup_session_streaming_messages()` |
| `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs` | Call cleanup after `claim_session_running` |

---

## Task 1: Add `truncate_chunk_content` to memory.rs

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/memory.rs`

Context: `handle_memory_search` (line 80) formats results at line 100–108. `handle_memory_get` (line 215) formats at line 225–236. Neither caps chunk size, so a 247 000-char Excalidraw file lands in the LLM context intact.

- [ ] **Step 1: Write the failing tests**

Add a `#[cfg(test)]` block at the bottom of `memory.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::truncate_chunk_content;

    #[test]
    fn excalidraw_marker_replaced() {
        let big = format!("excalidraw-plugin: parsed\n{}", "x".repeat(100_000));
        let out = truncate_chunk_content(&big);
        assert_eq!(out, "[Excalidraw diagram — binary content, skipped]");
    }

    #[test]
    fn excalidraw_view_marker_replaced() {
        let big = "== EXCALIDRAW VIEW ==\nsome data";
        let out = truncate_chunk_content(big);
        assert_eq!(out, "[Excalidraw diagram — binary content, skipped]");
    }

    #[test]
    fn long_text_truncated_to_limit() {
        let long = "a".repeat(10_000);
        let out = truncate_chunk_content(&long);
        assert_eq!(out.len(), super::MEMORY_CHUNK_MAX_CHARS);
    }

    #[test]
    fn short_text_unchanged() {
        let short = "hello world";
        assert_eq!(truncate_chunk_content(short), short);
    }

    #[test]
    fn exactly_at_limit_unchanged() {
        let at_limit = "b".repeat(super::MEMORY_CHUNK_MAX_CHARS);
        let out = truncate_chunk_content(&at_limit);
        assert_eq!(out.len(), super::MEMORY_CHUNK_MAX_CHARS);
    }
}
```

- [ ] **Step 2: Run tests — expect compile failure (function not defined yet)**

```bash
cargo test -p hydeclaw-core --lib memory 2>&1 | grep -E "error|FAILED|passed"
```

Expected: compilation error `cannot find function \`truncate_chunk_content\``.

- [ ] **Step 3: Add the constant and function to memory.rs**

Insert after line 5 (`use crate::agent::memory_service::MemoryService;`) and before the first `//` comment block:

```rust
/// Maximum characters per memory chunk returned to the LLM.
/// Prevents context overflow from large documents (Excalidraw, logs, etc.).
pub(crate) const MEMORY_CHUNK_MAX_CHARS: usize = 6_000;

/// Truncate a single memory chunk's content to fit within context budget.
///
/// Excalidraw documents are detected by their file header and replaced with a
/// short placeholder — they are binary drawings that are meaningless as text.
/// Other content is hard-capped at `MEMORY_CHUNK_MAX_CHARS` by Unicode scalar
/// boundary (never splits a multi-byte character).
pub(crate) fn truncate_chunk_content(content: &str) -> &str {
    if content.contains("excalidraw-plugin: parsed")
        || content.contains("== EXCALIDRAW VIEW ==")
    {
        return "[Excalidraw diagram — binary content, skipped]";
    }
    let limit = content.floor_char_boundary(MEMORY_CHUNK_MAX_CHARS.min(content.len()));
    &content[..limit]
}
```

- [ ] **Step 4: Run tests — expect 5 passed**

```bash
cargo test -p hydeclaw-core --lib memory 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok. 5 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/memory.rs
git commit -m "feat(memory): add truncate_chunk_content with Excalidraw detection and 6k char cap"
```

---

## Task 2: Apply truncation in `handle_memory_search`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/memory.rs` (lines 100–108)

- [ ] **Step 1: Replace the format line in `handle_memory_search`**

Current code (lines 100–108):
```rust
let body = results
    .iter()
    .enumerate()
    .map(|(i, r)| {
        let pin = if r.pinned { "\u{1f4cc} " } else { "" };
        format!("{}. [{}] {}{}  (id: {})", i + 1, r.source, pin, r.content, r.id)
    })
    .collect::<Vec<_>>()
    .join("\n");
```

Replace with:
```rust
let body = results
    .iter()
    .enumerate()
    .map(|(i, r)| {
        let pin = if r.pinned { "\u{1f4cc} " } else { "" };
        let content = truncate_chunk_content(&r.content);
        format!("{}. [{}] {}{}  (id: {})", i + 1, r.source, pin, content, r.id)
    })
    .collect::<Vec<_>>()
    .join("\n");
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p hydeclaw-core 2>&1 | grep -E "error|warning.*unused"
```

Expected: no errors.

- [ ] **Step 3: Run all lib tests**

```bash
cargo test -p hydeclaw-core --lib 2>&1 | grep "test result"
```

Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/memory.rs
git commit -m "fix(memory): truncate search result chunks to prevent context overflow"
```

---

## Task 3: Apply truncation in `handle_memory_get`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/memory.rs` (lines 225–236)

- [ ] **Step 1: Replace the format line in `handle_memory_get`**

Current code (lines 225–236):
```rust
Ok(chunks) => chunks
    .iter()
    .map(|c| {
        let pin = if c.pinned { "\u{1f4cc} " } else { "" };
        format!(
            "[{}] {}(score:{:.2}) {}\n  id: {} | created: {}",
            c.source, pin, c.relevance_score, c.content,
            c.id, c.created_at.format("%Y-%m-%d %H:%M")
        )
    })
    .collect::<Vec<_>>()
    .join("\n\n"),
```

Replace with:
```rust
Ok(chunks) => chunks
    .iter()
    .map(|c| {
        let pin = if c.pinned { "\u{1f4cc} " } else { "" };
        let content = truncate_chunk_content(&c.content);
        format!(
            "[{}] {}(score:{:.2}) {}\n  id: {} | created: {}",
            c.source, pin, c.relevance_score, content,
            c.id, c.created_at.format("%Y-%m-%d %H:%M")
        )
    })
    .collect::<Vec<_>>()
    .join("\n\n"),
```

- [ ] **Step 2: Compile and test**

```bash
cargo check -p hydeclaw-core 2>&1 | grep "error"
cargo test -p hydeclaw-core --lib 2>&1 | grep "test result"
```

Expected: no errors, all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/memory.rs
git commit -m "fix(memory): truncate get result chunks to prevent context overflow"
```

---

## Task 4: Add `cleanup_session_streaming_messages` to sessions.rs

**Files:**
- Modify: `crates/hydeclaw-db/src/sessions.rs` (after `claim_session_running` at line 449)

Context: `cleanup_interrupted_sessions` (line 738) does a global batch cleanup at startup. We need the same operation scoped to a single session, called at re-entry time.

- [ ] **Step 1: Write a unit test for the new function**

The existing test suite for sessions.rs uses integration tests with a real DB. Add this test to the `#[cfg(test)]` block in `sessions.rs` (or the nearest integration test file that sets up a DB connection — check `crates/hydeclaw-db/tests/` or `crates/hydeclaw-core/tests/`). If no unit test block exists in sessions.rs, add one.

If running as an integration test is required, mark with `#[ignore]` and note it is verified by the bootstrap integration path. For a quick compile check, add a doc-test instead:

In the function's doc comment:
```rust
/// Returns the number of rows updated (0 if none were streaming).
///
/// # Example (integration only — requires DB)
/// ```ignore
/// let count = cleanup_session_streaming_messages(&pool, session_id).await?;
/// assert_eq!(count, 0); // no streaming messages → no-op
/// ```
```

- [ ] **Step 2: Add the function after `claim_session_running` (around line 458)**

```rust
/// Mark any `status='streaming'` messages in `session_id` as `'interrupted'`.
///
/// Called in bootstrap just after `claim_session_running` so that a streaming
/// message left by a previous crashed run does not pollute the context of the
/// new run. Returns the number of rows updated (0 if none were streaming).
pub async fn cleanup_session_streaming_messages(
    db: &PgPool,
    session_id: Uuid,
) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        "UPDATE messages SET status = 'interrupted'
         WHERE session_id = $1 AND status = 'streaming'",
        session_id
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p hydeclaw-db 2>&1 | grep "error"
```

Expected: no errors.

- [ ] **Step 4: Run all lib tests**

```bash
cargo test -p hydeclaw-db --lib 2>&1 | grep "test result"
cargo test -p hydeclaw-core --lib 2>&1 | grep "test result"
```

Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-db/src/sessions.rs
git commit -m "feat(db): add cleanup_session_streaming_messages for per-session orphan cleanup"
```

---

## Task 5: Call cleanup in `bootstrap.rs`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs` (after line 102)

Context: `claim_session_running` is called at line 94. Cleanup must happen after the session is claimed (line 102, the `Ok(true) => {}` branch) and before the lifecycle guard is created (line 112) and context is loaded.

- [ ] **Step 1: Add the import if needed**

Check the existing imports at the top of `bootstrap.rs`. If `crate::db::sessions` is not already imported for other symbols, confirm the call path. The function is called as `crate::db::sessions::cleanup_session_streaming_messages(...)` — same pattern as `claim_session_running` already used on line 94.

- [ ] **Step 2: Insert the cleanup call after line 102**

Current code around lines 94–103:
```rust
match crate::db::sessions::claim_session_running(&engine.cfg().db, session_id).await {
    Ok(true) => {}
    Ok(false) => {
        anyhow::bail!("session {} not found; bootstrap aborted", session_id);
    }
    Err(e) => {
        tracing::warn!(session_id = %session_id, error = %e, "claim_session_running failed");
    }
}
log_wal_running_with_retry(&sm, session_id).await;
```

Replace with:
```rust
match crate::db::sessions::claim_session_running(&engine.cfg().db, session_id).await {
    Ok(true) => {}
    Ok(false) => {
        anyhow::bail!("session {} not found; bootstrap aborted", session_id);
    }
    Err(e) => {
        tracing::warn!(session_id = %session_id, error = %e, "claim_session_running failed");
    }
}

// Clean up any streaming message left by a previous crashed run.
// Must run after claim_session_running and before context is loaded.
match crate::db::sessions::cleanup_session_streaming_messages(&engine.cfg().db, session_id).await {
    Ok(0) => {}
    Ok(n) => tracing::info!(session=%session_id, count=%n, "cleaned orphaned streaming messages"),
    Err(e) => tracing::warn!(session=%session_id, error=%e, "cleanup_session_streaming_messages failed"),
}

log_wal_running_with_retry(&sm, session_id).await;
```

Note: errors are logged but not propagated — a cleanup failure must not prevent the session from starting.

- [ ] **Step 3: Compile and run all tests**

```bash
cargo check --all-targets 2>&1 | grep "error"
cargo test --lib 2>&1 | grep "test result"
```

Expected: no errors, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs
git commit -m "fix(bootstrap): cleanup orphaned streaming messages on session re-entry"
```

---

## Task 6: Final verification and push

- [ ] **Step 1: Full cargo check**

```bash
cargo check --all-targets 2>&1 | grep -E "^error"
```

Expected: no output (no errors).

- [ ] **Step 2: Full unit test run**

```bash
cargo test --lib 2>&1 | grep -E "test result|FAILED"
```

Expected: all crates show `test result: ok`.

- [ ] **Step 3: Push**

```bash
git push origin master
```

- [ ] **Step 4: Verify CI**

```bash
gh run list --limit 2
```

Wait for the run to complete, then:

```bash
gh run view $(gh run list --limit 1 --json databaseId --jq '.[0].databaseId') 2>&1 | grep -E "✓|✗|X |success|failure"
```

Expected: all jobs show ✓.
