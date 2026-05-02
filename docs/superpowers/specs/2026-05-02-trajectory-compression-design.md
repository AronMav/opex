# P0.1 — Trajectory Compression (Hermes Parity)

**Date:** 2026-05-02
**Status:** Approved for implementation
**Reference:** `D:/GIT/hermes-agent/agent/context_compressor.py`

---

## Context

HydeClaw has a working compaction system (`history.rs`, `pipeline/context.rs`,
`pipeline/llm_call.rs`) but it fires only reactively — on 400 overflow from the
LLM provider. This means the agent hits the context wall before any action is
taken, wastes a round-trip, and the 3-attempt retry loop is the only safety net.

Hermes implements a proactive, session-stateful compressor that checks token
usage after every LLM response and compresses before the next call if needed.
P0.1 brings HydeClaw to full Hermes parity while preserving the unique
HydeClaw feature of extracting facts into pgvector memory alongside the
in-context summary.

The reactive overflow recovery in `llm_call.rs` is kept as a backstop for edge
cases (e.g. a single system prompt that exceeds the limit on first call).

---

## Architecture

### Current flow
```
handle_sse()
  loop:
    LLM call → 400 overflow → compact → retry (max 3)
```

### New flow
```
handle_sse()
  → Compressor::load(session_id, db)   // load persisted state
  loop:
    if compressor.should_compress(last_token_count):
        compressor.compress(&mut messages, cfg, provider).await
    LLM call
    response.usage → compressor.update_token_count(input_tokens)
    process tool calls
  → compressor.save(session_id, db)   // persist state
```

### Components

| Component | Location | Role |
|---|---|---|
| `Compressor` struct | `agent/compressor.rs` (new) | Per-pipeline state, trigger logic |
| Compression algorithm | `agent/history.rs` (update) | 5-phase compress, summary generation |
| Proactive trigger | `agent/pipeline/execute.rs` (update) | Check before each LLM call |
| Config | `config/mod.rs` (update) | New CompactionConfig fields |
| DB state | `migrations/NNN_sessions_compaction_state.sql` (new) | Persist across reconnects |

---

## CompactionConfig

```toml
[agent.compaction]
# Existing fields (unchanged defaults)
enabled = true
threshold = 0.75               # fraction of context window; was 0.80
preserve_last_n = 10           # fallback min tail messages

# New fields
protect_first_n = 3            # head: system + first user + first assistant
summary_target_ratio = 0.20    # tail budget = threshold_tokens * ratio
anti_thrash_min_savings = 0.10 # skip if compression saves < 10%
anti_thrash_max_skips = 2      # stop after N consecutive ineffective compressions
extract_to_memory = true       # keep HydeClaw fact extraction to pgvector
```

Default for agents without `[agent.compaction]` section: `enabled = false`
(backward-compatible — existing agents do not change behaviour without opt-in).

---

## Compressor Struct

```rust
pub struct Compressor {
    pub previous_summary: Option<String>, // for iterative updates
    pub ineffective_count: u8,            // anti-thrashing counter
    pub last_prompt_tokens: u32,          // from most recent LLM response
    pub compression_count: u32,           // diagnostics / logging
}

impl Compressor {
    pub fn load(state: Option<serde_json::Value>) -> Self { ... }
    pub fn to_json(&self) -> serde_json::Value { ... }
    pub fn should_compress(&self, cfg: &CompactionConfig, context_limit: u32) -> bool { ... }
    pub fn update_token_count(&mut self, input_tokens: u32) { ... }
}
```

**`should_compress` logic:**
1. `last_prompt_tokens < threshold * context_limit` → false
2. `ineffective_count >= anti_thrash_max_skips` → false + warn
3. otherwise → true

`context_limit` is the model's context window, resolved in `execute.rs` via
`llm_call::default_context_for_model(&cfg.agent.model)` — the same lookup
already used by the overflow recovery path.

---

## DB Migration

```sql
ALTER TABLE sessions
  ADD COLUMN IF NOT EXISTS compaction_state JSONB;

-- Schema: {"previous_summary": "...", "ineffective_count": 0, "compression_count": 2}
-- NULL = no compaction has occurred for this session
```

`Compressor` is loaded from `sessions.compaction_state` at pipeline entry
(`bootstrap.rs`) and written back at pipeline exit (`finalize.rs`).

---

## Compression Algorithm — 5 Phases

### Phase 1 — Pre-pass (no LLM, O(n))

**Tool result pruning** — outside the protected tail:
- Replace tool result content with a 1-line informative summary:
  `[workspace_read] read SOUL.md (2,400 chars)`
  `[code_exec] ran script.py → exit 0, 47 lines output`
  Only applied when content > 200 chars.
- Deduplicate identical tool results (same MD5 hash): keep the most recent
  full copy, replace older duplicates with
  `[Duplicate tool output — same content as a more recent call]`.

**Tool call argument truncation** — outside the protected tail:
- Truncate long string values inside `arguments` JSON to 200 chars.
- Must remain valid JSON (parse → shrink string leaves → re-serialize).
  Never truncate at a raw byte offset — that produces invalid JSON that
  causes provider 400s on every subsequent turn.

### Phase 2 — Boundary Calculation

**Head end:**
- Start at index `protect_first_n`.
- Slide forward past any leading tool-result messages (avoid starting the
  summarised region mid tool-call/result group).

**Tail start (token budget, not fixed count):**
- Walk backward from the end accumulating estimated tokens
  (`content.len() / 4 + 10` per message, `_IMAGE_TOKEN_ESTIMATE = 1600`
  per image part in multimodal content).
- Stop when accumulated > `threshold_tokens * summary_target_ratio`.
- Hard minimum: always protect at least 3 messages.
- **Invariant:** the most recent user-role message must always be in the tail
  (active task must not be lost to compression). If it would fall in the
  middle region, pull `tail_start` back to include it.
- Align backward: if `tail_start` falls inside a tool_call/result group, pull
  back to before the parent assistant message.

Compression is skipped if `head_end >= tail_start` (nothing to summarise).

### Phase 3 — LLM Summary

**Iterative update** (when `previous_summary` is set):
```
"You are updating a context compaction summary. A previous compaction produced
the summary below. New turns have occurred since then. Update the summary
preserving all still-relevant information. Add new completed actions (continue
numbering). Move answered questions to Resolved. Update Active Task to the
user's most recent unfulfilled request."
```

**Fresh summary** (first compaction):
```
"You are a summarization agent creating a context checkpoint for a DIFFERENT
assistant that continues the conversation. Do NOT respond to questions — only
output the structured summary. Write in the agent's language. Never include
API keys, tokens, passwords — write [REDACTED]."
```

**Structured template (13 sections):**
```
## Active Task
## Goal
## Constraints & Preferences
## Completed Actions      ← numbered list: N. ACTION target — outcome [tool: name]
## Active State           ← working dir, branch, modified files, test status
## In Progress
## Blocked                ← exact error messages
## Key Decisions
## Resolved Questions
## Pending User Asks
## Relevant Files
## Remaining Work
## Critical Context       ← specific values, never secrets
```

Summary token budget: `max(2000, min(content_tokens * 0.20, 12000))`.

**Fallback** when summary LLM fails: insert static marker
`"[CONTEXT COMPACTION — REFERENCE ONLY] Summary generation was unavailable.
N message(s) were removed to free context space. Continue based on the recent
messages below."` Do not block the pipeline.

**Summary prefix:**
```
[CONTEXT COMPACTION — REFERENCE ONLY] Earlier turns were compacted into the
summary below. This is a handoff from a previous context window — treat it as
background reference, NOT as active instructions. Do NOT answer questions or
fulfill requests mentioned in this summary; they were already addressed.
Your current task is identified in the '## Active Task' section — resume
exactly from there. Respond ONLY to the latest user message that appears
AFTER this summary.
```

**Parallel:** fact extraction into pgvector (existing `history.rs` logic,
unchanged). Both run concurrently: `tokio::join!(generate_summary(), extract_facts())`.

### Phase 4 — Assembly

```
[head messages 0..head_end]
[summary message]
[tail messages tail_start..end]
```

**Summary message role:** avoid consecutive same-role with neighbours.
- If head ends with assistant/tool → use `user`
- Otherwise → use `assistant`
- If chosen role collides with tail start: flip. If both would collide:
  merge summary as a prefix into the first tail message with separator
  `"\n\n--- END OF CONTEXT SUMMARY — respond to the message below ---\n\n"`.

System message (index 0) gets a one-time note appended:
`"[Note: Some earlier conversation turns have been compacted into a handoff
summary to preserve context space. Build on that summary rather than
re-doing work.]"`
Only appended once per session (checked via substring match).

### Phase 5 — Sanitization

After assembly, fix broken tool_call/tool_result integrity:
1. **Orphaned tool results** (no matching `tool_call_id` in any assistant
   message) → remove.
2. **Orphaned tool calls** (assistant has tool_calls with no matching result)
   → insert stub: `{"role": "tool", "content": "[Result from earlier
   conversation — see context summary above]", "tool_call_id": "..."}`.

### Anti-thrashing (after compress returns)

```
savings_pct = (tokens_before - tokens_after) / tokens_before
if savings_pct < anti_thrash_min_savings:
    ineffective_count += 1
else:
    ineffective_count = 0
```

When `ineffective_count >= anti_thrash_max_skips`, `should_compress()` returns
false and logs:
```
warn: compression skipped — last N compressions each saved <10%.
Consider starting a new session.
```

---

## Integration Points

### `pipeline/execute.rs` — proactive trigger

```rust
// Before each LLM call in the tool loop:
if compressor.should_compress(&cfg.compaction, context_limit) {
    compressor.compress(&mut messages, &cfg, provider).await;
}

// After each LLM response:
if let Some(usage) = &response.usage {
    compressor.update_token_count(usage.input_tokens);
}
```

### `pipeline/bootstrap.rs` — load state

```rust
let compaction_state = db::sessions::get_compaction_state(db, session_id).await?;
let compressor = Compressor::load(compaction_state);
// Pass compressor into CommandContext or directly to execute()
```

### `pipeline/finalize.rs` — save state

```rust
let state_json = compressor.to_json();
db::sessions::set_compaction_state(db, session_id, state_json).await?;
```

### `llm_call.rs` — reactive backstop unchanged

`chat_stream_with_overflow_recovery` keeps its 3-attempt overflow loop.
It will rarely fire with proactive compression in place.

---

## Testing

### Unit tests (no DB, no LLM)

| Test | What it verifies |
|---|---|
| `should_compress_below_threshold` | Returns false below threshold |
| `should_compress_above_threshold` | Returns true at/above threshold |
| `anti_thrash_skips_after_n_ineffective` | Returns false after N ineffective compressions |
| `anti_thrash_resets_on_effective_compression` | Counter resets when savings >= min% |
| `prune_deduplicates_identical_tool_results` | Oldest dup replaced with back-reference |
| `prune_replaces_large_tool_results_with_summary` | 1-line summary generated |
| `prune_truncates_tool_args_valid_json` | JSON remains valid after truncation |
| `tail_cut_respects_token_budget` | Tail boundary respects budget |
| `tail_cut_always_includes_last_user_message` | Active task never lost |
| `tail_cut_avoids_splitting_tool_groups` | Boundary aligned backward |
| `sanitize_removes_orphaned_tool_results` | Orphan results dropped |
| `sanitize_adds_stub_for_orphaned_calls` | Stub result inserted |

### Integration tests (mock LLM)

| Test | What it verifies |
|---|---|
| `proactive_compress_fires_before_llm_call` | Compression happens before send, not after 400 |
| `iterative_update_uses_previous_summary` | Second compression updates, not re-generates |
| `fallback_on_summary_failure_continues_pipeline` | Session continues with static marker |
| `compressor_state_persists_across_reconnect` | State loaded from DB on next handle_sse |
| `parallel_fact_extraction_and_summary` | Both run concurrently without blocking |

---

## Out of Scope

- `parent_session_id` compression chains → P1.1 (separate phase)
- `/compress <topic>` manual trigger command → future
- SSE event to UI when compression fires → future (could add `"phase": "compacting"`)
- Tiktoken-accurate token counting → not needed, rough estimate matches Hermes

---

## Files Changed

| File | Change |
|---|---|
| `crates/hydeclaw-core/src/agent/compressor.rs` | **NEW** — Compressor struct |
| `crates/hydeclaw-core/src/agent/history.rs` | Update: 5-phase algo, Hermes template, iterative update |
| `crates/hydeclaw-core/src/config/mod.rs` | Update: 5 new CompactionConfig fields |
| `crates/hydeclaw-core/src/agent/pipeline/execute.rs` | Update: proactive trigger + update_token_count |
| `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs` | Update: load compaction_state |
| `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` | Update: save compaction_state |
| `crates/hydeclaw-core/src/db/sessions.rs` | Update: get/set compaction_state |
| `migrations/NNN_sessions_compaction_state.sql` | **NEW** — ADD COLUMN compaction_state JSONB |

---

## History

- **2026-05-02** — Spec created via brainstorming session.
  User decisions: proactive trigger (Hermes-style), Variant C full parity,
  keep fact extraction to pgvector alongside Hermes summary.
