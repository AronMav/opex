# Memory Extraction Cleanup — Design Spec

**Problem:** Long-term memory accumulates noise after sessions: meta-commentary ("user asked to do X"), session-specific actions ("12 records were deleted"), and near-duplicate facts from rephrased extractions. Three symptoms: excess volume, low quality, duplicates.

**Decision:** `auto:session:*` entries serve no purpose if deleted immediately after rolling summary update — they are an intermediate computation artifact, not searchable data. The correct architecture removes them from the persistence layer entirely.

---

## Architecture

**Before:**
```
session done → LLM extracts facts → memory_chunks(auto:session:*) → rolling summary update
```

**After:**
```
session done → LLM extracts facts (in-memory only) → rolling summary update
```

Rolling summary (`source="rolling_summary:{agent_name}"`) is the single persistent extraction artifact. Individual session facts are transient — used to update the summary, never written to `memory_chunks`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/hydeclaw-core/src/agent/knowledge_extractor.rs` | Remove individual fact persistence; improve extraction prompt; remove `tool_insights` from schema; delete dead code |
| DB (one-time) | `DELETE FROM memory_chunks WHERE source LIKE 'auto:session:%'` — run on deploy |

---

## Extraction Schema

Remove `tool_insights` category. New schema:

```rust
struct ExtractedKnowledge {
    user_facts: Vec<String>,
    outcomes: Vec<String>,
    feedback: Vec<String>,
}
```

`tool_insights` was the noisiest category and has no destination without individual storage.
`update_rolling_summary` already skips `tool_insights` — removing the field does not affect it.

---

## Extraction Prompt Rules

Replace current vague "only extract non-trivial information" with explicit filters:

**Categories:**
- `user_facts` — stable facts about the user: preferences, domain knowledge, long-term goals, identity. Must remain relevant 6 months from now.
- `outcomes` — durable decisions, agreements, or corrections that affect future sessions.
- `feedback` — user's explicit reactions: what they approved, rejected, asked to redo.

**Rules:**
- **Timeless test:** would this fact still matter in 6 months? If no → skip it.
- **No session actions:** do not extract what happened in this session (actions taken, requests made, things fixed/deleted/deployed).
- **No implied facts:** do not extract facts implied by the conversation topic itself.
- **Self-contained:** each item must make sense without reading the session.
- **Maximum 3 items per category** (was 5 — reduces pressure to manufacture facts).
- **Return empty arrays** if nothing passes the timeless test.

---

## Code Changes — `knowledge_extractor.rs`

### 1. Remove individual fact persistence block (lines 148–183)

The entire block starting at `let mut saved = 0u32;` through the closing `tracing::info!` of the save loop is deleted. Only the rolling summary call remains:

```rust
// DELETE lines 148–183 (saved counter, source_prefix, four category loops, info log)

// KEEP:
update_rolling_summary(agent_name, provider, memory_store, &extracted).await;
```

### 2. Delete dead code

After removing the persistence block, the following become dead code and must be deleted:

- `save_if_new` (lines ~339–347)
- `save_if_new_with_provider` (lines ~349–395)
- `resolve_conflict` (lines ~397–450) — only called from `save_if_new_with_provider`
- `DEDUP_THRESHOLD` constant (line 23) — only referenced in `save_if_new_with_provider`
- All `save_if_new` unit tests (lines ~595–827, 8 `#[tokio::test]` functions)

### 3. Update `parse_extraction` tests

Tests at lines ~515–591 reference `result.tool_insights` directly (e.g. asserting it is empty or has expected values). These will fail to compile after `tool_insights` is removed from `ExtractedKnowledge`. Update each assertion to remove the `tool_insights` field access.

---

## One-Time DB Cleanup

Run on deploy (before service restart):

```sql
DELETE FROM memory_chunks WHERE source LIKE 'auto:session:%';
```

Can be run directly on Pi via `docker exec` into Postgres container. No migration required — data cleanup only.

---

## Testing

### Tests deleted
All `save_if_new` / `save_if_new_with_provider` tests (~lines 595–827) — deleted with the functions.

### Tests updated
`parse_extraction` tests (~lines 515–591) — remove `tool_insights` field assertions.

### New tests

```rust
#[test]
fn extracted_knowledge_schema_has_no_tool_insights() {
    let json = r#"{"user_facts":["x"],"outcomes":[],"feedback":[]}"#;
    let parsed: ExtractedKnowledge = serde_json::from_str(json).unwrap();
    let _ = parsed;
    // Compile-time guarantee: code won't compile if tool_insights field is re-added
}
```

### Verification on Pi (after deploy)

```bash
# Before cleanup — count existing auto:session:* entries:
docker exec $(docker ps -q --filter name=postgres) \
  psql -U hydeclaw -d hydeclaw \
  -c "SELECT COUNT(*) FROM memory_chunks WHERE source LIKE 'auto:session:%';"

# After deploy + cleanup SQL — must return 0

# After one complete session — verify:
# 1. No new auto:session:* entries appear
# 2. rolling_summary chunk updated (source = 'rolling_summary:AgentName')
```

---

## What Does NOT Change

- `update_rolling_summary` logic — unchanged; calls `memory_store.index()` directly
- `CONFLICT_THRESHOLD: 0.5` — kept (used inside `update_rolling_summary`)
- `MIN_MESSAGES: 5` — kept
- `MAX_CONTEXT_MESSAGES: 20` — kept
- Workspace file indexing (`scope="shared"` from watcher) — unaffected
- Manual `memory_index` tool calls by agent — unaffected
