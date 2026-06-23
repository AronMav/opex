# P0.2 — Session Skill Review (Hermes Parity)

**Date:** 2026-05-03  
**Status:** Approved for implementation

---

## Context

OPEX has a working skill evolution system (`skills/evolution.rs`) that analyzes
completed heartbeat tasks and queues skill repairs via `pending_skill_repairs`. However,
it fires **only after scheduled heartbeat tasks** — never after interactive sessions.

Hermes runs a background skill review after every user-facing turn (every 10 tool
iterations), using a forked agent with a rich `_SKILL_REVIEW_PROMPT`. This captures
patterns, corrections, and new workflows from real usage rather than waiting for
scheduled tasks.

P0.2 brings OPEX to Hermes parity for session-based skill evolution while
keeping the existing curator pipeline (queue → Hyde writes files) intact.

---

## Architecture

```
finalize.rs::finalize()
  └─ spawn_skill_review()              ← new, alongside spawn_knowledge_extraction
       └─ evolution::review_session_for_skills(
              db, provider, agent_name, session_id
          )
            ├─ load messages from DB (user + assistant only, last 30)
            ├─ build task_summary from all user messages
            ├─ call LLM with session-specific prompt
            │   → SKIP / FIX / DERIVED / CAPTURED
            └─ enqueue → pending_skill_repairs (existing queue)
```

The result lands in the existing `pending_skill_repairs` queue. The weekly curator
processes it via Hyde — no new file-writing path needed.

---

## Configuration

New TOML section per agent, independent of `[agent.compaction]`:

```toml
[agent.skill_review]
enabled = true
min_tool_calls = 3    # minimum tool_end WAL events to trigger review
```

`AgentConfig` gains a new optional field:

```rust
pub skill_review: Option<SkillReviewConfig>,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "SkillReviewConfig::default_min_tool_calls")]
    pub min_tool_calls: u32,
}

impl SkillReviewConfig {
    fn default_min_tool_calls() -> u32 { 3 }
}

impl Default for SkillReviewConfig {
    fn default() -> Self { Self { enabled: false, min_tool_calls: 3 } }
}
```

Default: `enabled = false` (backward-compatible — agents without the section are unaffected).

---

## Trigger in `finalize.rs`

`tool_call_count` is fetched from WAL with the query already used for failure logging
(`COUNT(*) WHERE event_type = 'tool_end' AND session_id = $1`). A helper
`count_tool_calls(db, session_id)` extracts it for reuse.

**Note:** The exact field path for agent config in `FinalizeContext` must be verified
against the struct definition before implementation — it may be `ctx.agent_cfg` or
`ctx.cfg` rather than `ctx.engine_cfg`.

```rust
// In finalize(), after spawn_knowledge_extraction:
// (verify exact field name for agent config in FinalizeContext)
if let (Some(sr_cfg), FinalizeOutcome::Done) = (&ctx.agent_cfg.agent.skill_review, &outcome) {
    if sr_cfg.enabled {
        let tool_count = count_tool_calls(&ctx.db, ctx.session_id).await.unwrap_or(0);
        if tool_count >= sr_cfg.min_tool_calls {
            spawn_skill_review(
                ctx.bg_tasks.clone(),
                ctx.db.clone(),
                ctx.provider.clone(),
                ctx.agent_name.clone(),
                ctx.session_id,
            );
        }
    }
}
```

`spawn_skill_review` uses `bg_tasks.spawn()` — fire-and-forget, awaited on graceful
shutdown, same pattern as `spawn_knowledge_extraction`.

---

## New Function: `review_session_for_skills`

Location: `crates/opex-core/src/skills/evolution.rs`

```rust
pub async fn review_session_for_skills(
    db: &PgPool,
    provider: &Arc<dyn LlmProvider>,
    agent_name: &str,
    session_id: Uuid,
)
```

### Steps

1. **Load messages** — `db::sessions::load_messages(db, session_id, None)`, filter to
   `user` and `assistant` roles, take last 30.

2. **Build `task_summary`** — concatenate all user message contents with `"\n---\n"`,
   truncate to 2 000 characters using `floor_char_boundary(2000)` to preserve
   valid UTF-8 (never truncate mid-codepoint). Only user messages are included —
   assistant responses are intentionally excluded to keep the summary compact and
   avoid exceeding the analyzer's context.

3. **Load available skill names** — same as `analyze_and_evolve`: non-archived skills
   from workspace. If empty, uses `"none"`.

4. **Call LLM** with the session prompt (see below). Single turn, no tools.
   Wrap with `tokio::time::timeout(Duration::from_secs(30), ...)` — on timeout,
   log `tracing::warn!` and return without enqueuing.

5. **Parse and enqueue** — same logic as `analyze_and_evolve`:
   - `SKIP` → return
   - `FIX <name>` → `skill_repairs::enqueue(..., "fix", line)`
   - `DERIVED <parent> <new>` → `skill_repairs::enqueue(..., "derived", line)`
   - `CAPTURED <new>` → `skill_repairs::enqueue(..., "captured", line)`

6. Log: `tracing::info!(agent, verdict, "session skill review complete")`.
   Errors: `tracing::debug!` only — never propagate.

### Prompt

```
You are a skill evolution analyzer reviewing a completed interactive session.
Agent: {agent_name}
User requests this session (summary):
{task_summary}

Available skill names (ONLY use these exact names): {available_str}

Respond with EXACTLY ONE line:
- SKIP — session was casual, no reusable pattern emerged
- FIX <skill_name> — an existing skill has a gap or error revealed by this session
  (skill_name MUST be from the list above)
- DERIVED <parent_skill> <new_name> — create a specialized variant of an existing skill
- CAPTURED <new_name> — a genuinely new reusable workflow or technique appeared

Act when:
  • The agent made a mistake the skill should prevent next time
  • The user corrected approach, style, format, or workflow
  • A non-trivial technique or pattern emerged that future sessions would benefit from
  • A loaded skill turned out to be wrong or incomplete

SKIP for casual conversation, one-off lookups, or sessions with no learnable pattern.
SKIP is a valid outcome — do not force an action where none fits.
```

The prompt is intentionally more conservative than Hermes (SKIP is not penalised)
because interactive sessions have more noise than scheduled tasks.

---

## Files Changed

| File | Change |
|---|---|
| `crates/opex-core/src/config/mod.rs` | Add `SkillReviewConfig`, `AgentConfig.skill_review` field |
| `crates/opex-core/src/skills/evolution.rs` | Add `review_session_for_skills()` |
| `crates/opex-core/src/agent/pipeline/finalize.rs` | Add `count_tool_calls()`, `spawn_skill_review()`, trigger in `finalize()` |

---

## Testing

### Unit tests (no DB, no LLM)

| Test | Verifies |
|---|---|
| `skill_review_skipped_below_min_tool_calls` | `tool_call_count < min` → spawn not called |
| `task_summary_truncated_at_2000_chars` | long history truncated via `floor_char_boundary`, no panic on multi-byte chars |
| `skip_verdict_does_not_enqueue` | SKIP response → `pending_skill_repairs` empty |
| `captured_verdict_enqueues_repair` | CAPTURED → one row with kind="captured" |
| `fix_verdict_enqueues_repair` | FIX → one row with kind="fix", correct skill_name |

### Integration tests (mock LLM)

| Test | Verifies |
|---|---|
| `review_fires_after_done_session_with_tools` | `finalize()` calls spawn when Done + tool_count >= min |
| `review_skipped_when_config_disabled` | `enabled = false` → no DB rows |
| `review_skipped_when_outcome_is_failed` | Failed outcome → spawn not called |

---

## Known Limitations

- `task_summary` is a lossy concatenation — the LLM sees user intent but not tool
  outputs or assistant reasoning. Sufficient for SKIP/FIX/CAPTURED decisions;
  richer context not needed.
- Repair is queued, not immediate. Files are written only when curator runs
  (weekly by default). This is intentional — avoids writing during active sessions.

---

## Out of Scope

- Running curator immediately after queuing — P0.2 only queues, curator timing unchanged
- Per-session skill report visible to user — future
- Tracking which skills were loaded/used during a session — future (`skills_used` is
  passed as `&[]` for now, same as heartbeat calls that don't track this either)
