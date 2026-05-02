---
phase: 260502-u9v
plan: 01
subsystem: security/workspace
tags: [security, prompt-injection, workspace, observability, unicode]
dependency_graph:
  requires: []
  provides: [detect_prompt_injection pub API, load_workspace_prompt injection logging]
  affects: [crates/hydeclaw-core/src/tools/content_security.rs, crates/hydeclaw-core/src/agent/workspace.rs]
tech_stack:
  added: []
  patterns: [tracing::warn! structured logging, zero-width unicode detection, log-only security scanning]
key_files:
  modified:
    - crates/hydeclaw-core/src/tools/content_security.rs
    - crates/hydeclaw-core/src/agent/workspace.rs
decisions:
  - Collapsible nested if-in-if flattened to `&&` per clippy collapsible_if rule
  - scan_and_warn kept private (fn, not pub fn) - only used inside workspace module
  - Integration tests focus on return-value contract, not tracing::warn! emission (no test subscriber setup)
metrics:
  duration: ~15min
  completed: 2026-05-02
  tasks_completed: 2
  files_modified: 2
---

# Phase 260502-u9v Plan 01: Prompt Injection Scanner Publish Summary

One-liner: Published test-only `detect_prompt_injection` to module scope with zero-width/RTL/BOM unicode detection and wired it into `load_workspace_prompt` as log-only `tracing::warn!` per offending workspace file.

## What Was Done

### Task 1: Publish detect_prompt_injection with zero-width unicode detection

`detect_prompt_injection` moved from `#[cfg(test)] mod tests` to module scope in `crates/hydeclaw-core/src/tools/content_security.rs`.

**Module path:** `crate::tools::content_security::detect_prompt_injection`

**Signature (unchanged):** `pub fn detect_prompt_injection(text: &str) -> Vec<&'static str>`

**Label set (complete):**
- `ignore_previous_instructions`
- `disregard_previous`
- `forget_everything`
- `role_override`
- `new_instructions`
- `system_override`
- `xml_system_tags`
- `privilege_escalation`
- `dangerous_command`
- `zero_width_chars` ← new

**Zero-width detection:** Added `const ZERO_WIDTH_CHARS: &[char]` with 5 code points:
- `\u{200b}` — ZERO WIDTH SPACE
- `\u{200c}` — ZERO WIDTH NON-JOINER
- `\u{200d}` — ZERO WIDTH JOINER
- `\u{202e}` — RIGHT-TO-LEFT OVERRIDE
- `\u{feff}` — ZERO WIDTH NO-BREAK SPACE (BOM / ZWNBSP)

Scans the RAW text (not lowercased) — case folding is irrelevant for these code points.

### Task 2: Wire detect_prompt_injection into load_workspace_prompt

**Import added:** `use crate::tools::content_security::detect_prompt_injection;`

**Private helper added:**
```rust
fn scan_and_warn(agent_name: &str, file: &str, content: &str) {
    let matches = detect_prompt_injection(content);
    if !matches.is_empty() {
        tracing::warn!(
            agent = %agent_name,
            file = %file,
            patterns = %matches.join(","),
            "prompt injection patterns detected in workspace file (log-only, not blocked)"
        );
    }
}
```

**Warn message structure:**
- Level: `WARN`
- Fields: `agent` (agent name), `file` (filename), `patterns` (comma-joined label list)
- Message: `"prompt injection patterns detected in workspace file (log-only, not blocked)"`

**Call sites in `load_workspace_prompt`:**
1. Priority files loop (SOUL.md, IDENTITY.md, MEMORY.md) — `Ok(content)` arm
2. Extra agent `.md` files loop — `Ok(content)` arm
3. Shared root files loop (TOOLS.md, AGENTS.md, USER.md) — `Ok(content)` arm

All calls are before `append_with_limit` — scan runs purely for side-effect logging, no early return.

## Test Coverage Delta

### tools::content_security::tests (new tests added)
- `test_zero_width_space_detected` — U+200B triggers `zero_width_chars`
- `test_rtl_override_detected` — U+202E triggers `zero_width_chars`
- `test_bom_detected` — U+FEFF triggers `zero_width_chars`
- `test_clean_ascii_no_zero_width` — clean ASCII does NOT report `zero_width_chars`
- `test_combined_injection_and_zero_width` — combined input reports both labels, no duplicates

**Previously passing tests (all still pass):**
- `test_no_injection`
- `test_ignore_previous`
- `test_role_override`
- `test_system_tags`
- `test_wrap_external`

Total: 10 tests (5 old + 5 new)

### agent::workspace::tests (new tests added)
- `load_workspace_prompt_returns_content_even_with_injection_patterns` — SOUL.md with injection text → returned prompt contains it verbatim (log-only, never blocked)
- `load_workspace_prompt_returns_content_with_zero_width_chars` — MEMORY.md with `hello\u{200b}world` → returned prompt contains it verbatim (non-destructive)
- `load_workspace_prompt_clean_files_unchanged` — SOUL/IDENTITY/MEMORY with benign content → all three verbatim in returned prompt

Total: 30 tests (27 existing + 3 new)

## API / Behavior Contract Changes

- `detect_prompt_injection` is now reachable from production code via `crate::tools::content_security::detect_prompt_injection`. Previously it existed only inside `#[cfg(test)]` and was unreachable from non-test code.
- `load_workspace_prompt` now emits `tracing::warn!` for each file that triggers any injection pattern. Clean files produce no warnings. The assembled prompt content is **never** modified, blocked, or redacted.
- No public API changes, no function signature changes, no DB schema changes, no migration needed.
- `wrap_external_content` is unchanged.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Clippy] Collapsed nested if-in-if in detect_prompt_injection**
- **Found during:** Task 1 clippy check
- **Issue:** `if outer { if inner { ... } }` pattern triggers `collapsible_if` clippy lint with `-D warnings`
- **Fix:** Combined conditions into `if outer && inner { ... }`
- **Files modified:** `crates/hydeclaw-core/src/tools/content_security.rs`
- **Commit:** b6409b0 (included in task 2 commit)

## Known Stubs

None. Both tasks fully implemented with no placeholder data or TODO markers.

## Self-Check: PASSED

- `crates/hydeclaw-core/src/tools/content_security.rs` — FOUND, contains `pub fn detect_prompt_injection`
- `crates/hydeclaw-core/src/agent/workspace.rs` — FOUND, contains `detect_prompt_injection` import and `scan_and_warn`
- Commit `65615d3` — FOUND (Task 1: publish function + add zero-width detection)
- Commit `b6409b0` — FOUND (Task 2: wire into load_workspace_prompt)
- All 10 content_security tests pass
- All 30 workspace tests pass
- `cargo check --all-targets -p hydeclaw-core` clean (2 pre-existing warnings in unrelated files)
- clippy clean on touched files (`content_security.rs`, `workspace.rs`)
