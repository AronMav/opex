---
phase: 260430-orh-memory-extraction-cleanup-reapply-spec
plan: 01
subsystem: knowledge_extractor
tags: [memory, extraction, cleanup, rolling-summary]
key-files:
  modified:
    - crates/hydeclaw-core/src/agent/knowledge_extractor.rs
decisions:
  - "Individual session facts are NOT persisted to memory_chunks; only the rolling summary persists"
  - "CONFLICT_THRESHOLD removed: spec said it was used in update_rolling_summary but grep confirmed it was only in save_if_new_with_provider — deleted along with that function"
  - "ConflictDecision and parse_conflict_decision deleted: became dead code after resolve_conflict removal (not listed in spec but required by correctness)"
metrics:
  duration: ~15 minutes
  completed: 2026-04-30
---

# Phase 260430-orh Plan 01: Memory Extraction Cleanup (Reapply Spec) Summary

**One-liner:** Eliminated `auto:session:*` memory_chunks noise by removing the individual fact persistence block from `extract_and_save_inner`, deleting ~150 lines of dead dedup/conflict code, and tightening the extraction prompt to a 3-field, timeless-test-filtered schema.

## What Was Done

Reapplied the design from `docs/superpowers/specs/2026-04-30-memory-extraction-cleanup-design.md` to `knowledge_extractor.rs`, which had been overwritten by the `e002fa9` merge.

### Architecture change

**Before:** `session done → LLM extracts facts → memory_chunks(auto:session:*) → rolling summary update`

**After:** `session done → LLM extracts facts (in-memory only) → rolling summary update`

## Changes Applied

### Lines deleted: 444 lines removed, 21 inserted (net -423)

**Struct change:**
- Removed `tool_insights: Vec<String>` field from `ExtractedKnowledge`
- Schema is now exactly 3 fields: `user_facts`, `outcomes`, `feedback`

**Persistence block deleted (lines 147-183):**
- `let mut saved = 0u32;`
- `let source_prefix = format!("auto:session:{}", session_id);`
- Four category save loops (user_facts, outcomes, tool_insights, feedback)
- The `if saved > 0 { tracing::info!(...) }` block
- Comment renumbered from `// 7.` to `// 6.` for the rolling summary call

**Dead code deleted:**
- `const DEDUP_THRESHOLD: f64 = 0.9;` and doc comment
- `const CONFLICT_THRESHOLD: f64 = 0.5;` — spec said kept, but grep confirmed it was ONLY referenced in `save_if_new_with_provider`; deleted after verification
- `save_if_new` async fn (~9 lines)
- `save_if_new_with_provider` async fn (~48 lines)
- `resolve_conflict` async fn (~75 lines)
- `struct ConflictDecision` + `fn parse_conflict_decision` (~32 lines) — dead after resolve_conflict gone

**Extraction prompt updated:**
- Dropped `tool_insights` from JSON schema example
- Replaced vague "only extract non-trivial information" with explicit timeless-test rules
- Reduced maximum items per category from 5 to 3
- Added: no session actions, no implied facts, self-contained requirements

**File-level doc comment updated** to describe rolling-summary-only architecture.

### Tests deleted: 19

| Test | Reason |
|------|--------|
| `save_if_new_skips_short_text` | Deleted with save_if_new |
| `save_if_new_saves_valid_text` | Deleted with save_if_new |
| `save_if_new_accepts_private_scope` | Deleted with save_if_new |
| `save_if_new_accepts_shared_scope` | Deleted with save_if_new |
| `save_if_new_rejects_exactly_10_chars` | Deleted with save_if_new |
| `save_if_new_rejects_9_chars` | Deleted with save_if_new |
| `save_if_new_trims_whitespace` | Deleted with save_if_new |
| `save_if_new_unavailable_store_returns_false` | Deleted with save_if_new |
| `parse_conflict_update` | Deleted with parse_conflict_decision |
| `parse_conflict_add` | Deleted with parse_conflict_decision |
| `parse_conflict_noop` | Deleted with parse_conflict_decision |
| `parse_conflict_delete` | Deleted with parse_conflict_decision |
| `parse_conflict_with_think_blocks` | Deleted with parse_conflict_decision |
| `parse_conflict_malformed_defaults_to_add` | Deleted with parse_conflict_decision |
| `parse_conflict_with_markdown_fences` | Deleted with parse_conflict_decision |
| `parse_conflict_missing_target_defaults_to_zero` | Deleted with parse_conflict_decision |
| `parse_conflict_missing_reason` | Deleted with parse_conflict_decision |
| `rolling_summary_collects_only_user_facts_outcomes_feedback` | Referenced deleted tool_insights field |
| `rolling_summary_empty_when_only_tool_insights` | Referenced deleted tool_insights field |
| `extraction_scope_assignment` | Documented old 4-scope design |

**Total deleted: 19 tests**

### Tests updated: 6

Removed `result.tool_insights` assertion lines from:
- `parse_clean_json` — dropped `assert_eq!(result.tool_insights, vec!["API responded in 2s"])`
- `parse_with_surrounding_text` — dropped `assert_eq!(result.tool_insights, vec!["C"])`
- `parse_empty_arrays` — dropped `assert!(result.tool_insights.is_empty())`
- `parse_missing_fields_default_empty` — dropped `assert!(result.tool_insights.is_empty())`
- `parse_multiple_items_per_category` — dropped `assert_eq!(result.tool_insights.len(), 1)`

JSON input strings intentionally kept — serde ignores unknown fields by default, so old LLM responses with `tool_insights` still parse gracefully.

### Tests added: 1

`extracted_knowledge_schema_has_no_tool_insights` — compile-time guard ensuring the 3-field schema is enforced.

## Verification

- `cargo check -p hydeclaw-core --all-targets`: exit 0, no warnings
- `cargo test -p hydeclaw-core -- knowledge_extractor`: 18 tests pass, 0 fail
- `grep -n "save_if_new\|resolve_conflict\|DEDUP_THRESHOLD\|ConflictDecision\|parse_conflict_decision" knowledge_extractor.rs`: zero hits
- `grep -n "auto:session" knowledge_extractor.rs`: zero hits
- `grep -rn "save_if_new\|DEDUP_THRESHOLD\|resolve_conflict" crates/ --include='*.rs'`: zero hits (no other file referenced deleted symbols)

## update_rolling_summary: unchanged

The `update_rolling_summary` function (lines 135-216 in the new file) is byte-identical to the pre-change version. No modifications were made inside this function. The function signature, logic, retry loop, think-block stripping, and chunk deletion+save pattern are all unchanged.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] CONFLICT_THRESHOLD deleted despite spec saying "kept"**
- **Found during:** Task 1, Step 5 verification
- **Issue:** Spec line 151 states "CONFLICT_THRESHOLD: 0.5 — kept (used inside update_rolling_summary)". Grep showed it was ONLY defined at line 332 and used inside `save_if_new_with_provider` (line 379). It has zero references inside `update_rolling_summary`. After deleting `save_if_new_with_provider`, the constant became unreferenced.
- **Fix:** Deleted `const CONFLICT_THRESHOLD: f64 = 0.5;` per the plan's Step 5 instruction ("if zero hits remain outside the definition, delete it too — trust the compiler").
- **Files modified:** `knowledge_extractor.rs`
- **Commit:** 249065a

**2. [Rule 2 - Missing critical cleanup] ConflictDecision + parse_conflict_decision deleted**
- **Found during:** Task 1, Step 4
- **Issue:** Plan mentions deleting these in Step 4 but the spec doesn't list them explicitly. They only have callers inside `resolve_conflict`. After deleting `resolve_conflict`, they become dead code.
- **Fix:** Deleted both `struct ConflictDecision` and `fn parse_conflict_decision` along with their associated tests.
- **Commit:** 249065a

## Deploy Note (Out of Scope)

The one-time DB cleanup SQL must be run on the Pi before or after deploying the new binary:

```sql
DELETE FROM memory_chunks WHERE source LIKE 'auto:session:%';
```

This is idempotent and safe to run at any time. No migration file required.

## Commit

- `249065a` — `refactor(260430-orh-01): remove individual fact persistence, dead code cleanup in knowledge_extractor`

## Self-Check: PASSED

- File exists: `crates/hydeclaw-core/src/agent/knowledge_extractor.rs` — FOUND
- Commit exists: `249065a` — FOUND
- 18 tests pass, 0 fail
- cargo check exits 0
