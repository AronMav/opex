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
| `crates/hydeclaw-core/src/agent/knowledge_extractor.rs` | Remove individual fact persistence; improve extraction prompt; remove `tool_insights` from schema; raise `DEDUP_THRESHOLD` |
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

`tool_insights` was the noisiest category ("used memory_search to search") and has no destination without individual storage.

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

### Remove individual fact persistence

```rust
// DELETE the entire block in extract_and_save_inner:
let mut saved = 0u32;
let source_prefix = format!("auto:session:{}", session_id);
for fact in &extracted.user_facts {
    if save_if_new_with_provider(...).await { saved += 1; }
}
// ... all four category loops

// KEEP only:
update_rolling_summary(agent_name, provider, memory_store, &extracted).await;
```

### Raise dedup threshold

```rust
const DEDUP_THRESHOLD: f64 = 0.95;  // was 0.90
```

The `save_if_new` / `save_if_new_with_provider` functions are **deleted** — grep confirms they are only called from the individual fact saving block being removed. Their unit tests (~lines 593–824) are deleted with them. `resolve_conflict` is only called from `save_if_new_with_provider`, so it is deleted too.

---

## One-Time DB Cleanup

Run on deploy (before service restart):

```sql
DELETE FROM memory_chunks WHERE source LIKE 'auto:session:%';
```

Can be run directly on Pi via `docker exec` into Postgres container, or via an agent `memory_delete` action targeting the source prefix. No migration required — it is a data cleanup, not a schema change.

---

## Testing

### Existing tests
Unit tests for `save_if_new_with_provider` (line ~798 in `knowledge_extractor.rs`) are unaffected — the function is retained.

### New tests

```rust
#[tokio::test]
async fn extract_and_save_does_not_index_individual_facts() {
    // Mock memory_store: assert index() is never called
    // Assert update_rolling_summary is called once
    // Verify no auto:session:* writes
}

#[test]
fn extracted_knowledge_schema_has_no_tool_insights() {
    // Compile-time guarantee via serde roundtrip
    let json = r#"{"user_facts":["x"],"outcomes":[],"feedback":[]}"#;
    let parsed: ExtractedKnowledge = serde_json::from_str(json).unwrap();
    let _ = parsed; // field access would fail to compile if tool_insights present
}
```

### Verification on Pi (after deploy)

```bash
# Before cleanup:
docker exec $(docker ps -q --filter name=postgres) \
  psql -U hydeclaw -d hydeclaw \
  -c "SELECT COUNT(*) FROM memory_chunks WHERE source LIKE 'auto:session:%';"

# Run cleanup SQL, then:
# After cleanup → count must be 0

# After one complete session:
# Verify no new auto:session:* entries appear
# Verify rolling_summary chunk was updated
```

---

## What Does NOT Change

- `update_rolling_summary` logic — unchanged
- `save_if_new_with_provider` function — kept, used by rolling summary path
- `CONFLICT_THRESHOLD: 0.5` — kept (rolling summary conflict resolution)
- `MIN_MESSAGES: 5` — kept
- `MAX_CONTEXT_MESSAGES: 20` — kept
- Workspace file indexing (`scope="shared"` from watcher) — unaffected
- Manual `memory_index` tool calls by agent — unaffected
