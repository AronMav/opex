---
phase: 260504-fiz
plan: "01"
subsystem: scheduler
tags: [cron, delivery-routing, truncation, local-save, docs]
dependency_graph:
  requires: []
  provides: [parse_target_string, truncate_reply_for_channel, save_to_local, extended-dispatch-loop]
  affects: [scheduler/mod.rs, hermes-insights-plan.md]
tech_stack:
  added: []
  patterns: [string-enum-parsing, char-count-truncation, async-fs-write]
key_files:
  created: []
  modified:
    - crates/hydeclaw-core/src/scheduler/mod.rs
    - docs/specs/2026-04-30-hermes-insights-plan.md
decisions:
  - "Thread component in telegram:chat_id:thread_id silently dropped — stored as future work in doc comment"
  - "CHANNEL_MAX_CHARS = 4000, suffix starts with ellipsis character (not ASCII ...)"
  - "save_to_local not unit-tested — I/O surface requires real workspace, covered by integration/manual testing"
  - "garbage-items test renamed to _filtered to reflect normalize-time filtering (was dispatch-time)"
metrics:
  duration: ~20min
  completed: "2026-05-04"
  tasks_completed: 2
  files_modified: 2
---

# Phase 260504-fiz Plan 01: DeliveryTarget String Parsing & Truncation Guard Summary

P1.3 closure: `parse_target_string` + `truncate_reply_for_channel` + `save_to_local` + extended cron dispatch loop for local/origin/channel string targets with 4000-char truncation guard.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Extend scheduler: parse_target_string, truncation, local-save, dispatch loop | 1688718 | crates/hydeclaw-core/src/scheduler/mod.rs |
| 2 | Update Hermes-insights plan doc — P1.2/P1.3 done, P2.8/P2.9/P2.10 | 6615828 | docs/specs/2026-04-30-hermes-insights-plan.md |

## What Was Built

### New helpers in `scheduler/mod.rs`

**`parse_target_string(s: &str) -> Option<serde_json::Value>`**

Parses string-form delivery targets:
- `"local"` → `{"type": "local"}`
- `"origin"` → `{"type": "origin"}`
- `"telegram:99"` → `{"channel": "telegram", "chat_id": 99}` (i64)
- `"telegram:99:42"` → same, thread component silently dropped
- invalid input → `None`

**`normalize_announce_to(val: &serde_json::Value) -> Vec<serde_json::Value>`**

Extended to handle bare strings and string items in arrays via `parse_target_string`. Non-parseable array items (numbers, nulls) are now filtered at normalize-time instead of being passed through to the dispatch loop.

**`truncate_reply_for_channel(reply: &str) -> (String, bool)`**

Returns `(text_for_channel, needs_save)`. Short replies (≤ 4000 chars) pass through unchanged. Long replies are truncated to 4000 chars + `…\n\n[полный вывод сохранён в workspace]` suffix.

**`save_to_local(workspace_dir, agent_name, job_id, content) -> Option<String>`**

Async helper. Writes to `workspace/agents/{agent}/cron_output/{YYYYMMDDTHHMMSS}_{job_short}.txt`. Returns workspace-relative path on success, `None` on I/O error (non-fatal, logs warn).

### Updated dispatch loop (`add_dynamic_job`)

Old behavior: hard-coded `reply.chars().take(2000)` + only Object/Array-of-Object targets.

New behavior:
- `local` target — calls `save_to_local`, logs info, no channel send
- `origin` target — logs warning, skips (not yet supported for scheduled jobs)
- channel target — sends `truncate_reply_for_channel` result; if truncated and saved, appends `📄 path` footer
- `save_to_local` is called once if `needs_save || has_local` (not per-target)
- Mirror spawn preserved exactly

### Unit tests (10 new + 1 renamed)

New tests (all pass):
- `parse_target_string_local`
- `parse_target_string_origin`
- `parse_target_string_channel_only`
- `parse_target_string_channel_with_thread`
- `parse_target_string_invalid` (5 invalid inputs)
- `normalize_announce_to_bare_string_parsed`
- `normalize_announce_to_string_in_array`
- `truncate_reply_short`
- `truncate_reply_long`

Renamed: `normalize_announce_to_array_with_garbage_items_passes_through` → `normalize_announce_to_array_with_garbage_items_filtered` (asserts length 1, not 3).

### Doc changes (`docs/specs/2026-04-30-hermes-insights-plan.md`)

- P1.2 promoted from in-progress to `✅ DONE` with implementation notes
- P1.3 promoted from `⚠️ PARTIAL` to `✅ DONE` with 2026-05-04 implementation notes
- Comparison matrix: session mirroring row updated from `❌` to `✅ is_mirror + mirror_to_session (260504)`
- Added P2.8 (Microsoft Teams Adapter), P2.9 (Centralized Audio Routing), P2.10 (Model Config from UI Dashboard)
- Added 2026-05-04 history bullet

## Deviations from Plan

None — plan executed exactly as written.

The only adjustment was in the TDD test `truncate_reply_long`: the plan spec described `content_part.ends_with('…')` which was logically inconsistent with the implementation (suffix starts with `…`, so `content_part` is the 4000 bare chars before it). Fixed the test assertion to correctly verify 4000-char content before the suffix. The implementation itself matches the spec exactly.

## Verification

- `cargo test -p hydeclaw-core 2>&1 | grep scheduler::tests` — all 50 tests pass (including 10 new)
- `cargo clippy -p hydeclaw-core --lib -- -D warnings` — clean (no errors in scheduler/mod.rs)
- Doc verification script: `doc OK`
- Manual: `grep "### ⚠️ P1\.3"` returns 0 hits; `grep "P2.10"` returns 1 hit

## Self-Check: PASSED

- `crates/hydeclaw-core/src/scheduler/mod.rs` — exists, modified
- `docs/specs/2026-04-30-hermes-insights-plan.md` — exists, modified
- Commit `1688718` — exists
- Commit `6615828` — exists
