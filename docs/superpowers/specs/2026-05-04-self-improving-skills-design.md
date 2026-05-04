# Self-Improving Skills — Design Spec

**Date:** 2026-05-04
**Status:** Approved

## Problem

HydeClaw's skill system is passive: the only way a skill is created is via the
Curator's Phase 2 repair queue (which is driven by a low-context post-session
LLM call on user messages only). Agents cannot create skills while working, and
the post-session analysis misses assistant responses, tool usage, and session
outcome — context that reveals whether something actually worked or failed.

## Goal

1. Add `skill_use(action="capture")` — agents create skills immediately during
   a session, with notification and Curator traceability.
2. Keep `skill_use(action="load")` unchanged (existing behaviour).
3. Enrich `review_session_inner()` to include assistant responses, tool call
   names, session outcome, and allow up to 3 verdicts per session.

---

## Part 1 — In-Session Skill Capture

### Tool definition

Extend the existing `skill_use` system tool with a new `capture` action:

| Action | Description |
| --- | --- |
| `load` | Load a skill's instructions into context (unchanged) |
| `capture` | Create a new skill file immediately from session learning |

The tool remains a system tool (hardcoded, not YAML). No rename — `skill_use`
stays as-is everywhere: allowlists, context_builder, agent prompts.

**Subagent restriction:** `skill_use` is already in the subagent deny-list
(`subagent.rs:352`). Subagents cannot call `capture` — intentional, since
subagents should not create global workspace skills.

### `capture` parameters

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `name` | string | yes | Kebab-case identifier. Validated against `[a-z0-9][a-z0-9-]*`. |
| `description` | string | yes | One-sentence summary of what the skill teaches. |
| `triggers` | string | no | Comma-separated phrases that should activate the skill. |
| `tools_required` | string | no | Comma-separated tool names this skill depends on. |
| `instructions` | string | yes | Full skill body in markdown. |

### `capture` execution flow

1. **Validate name** — regex `^[a-z0-9][a-z0-9-]*$`. Return error if invalid.
2. **Check for collision** — if `workspace/skills/{name}.md` already exists,
   return error: `"Skill '{name}' already exists. Use skill_use(action='load')
   to read it, or propose a different name."`.
3. **Build frontmatter** — `SkillFrontmatter { name, description,
   triggers: split(","), tools_required: split(","), priority: 5,
   state: Active, pinned: None, last_used_at: None }`.
   Priority 5 is a reasonable default; agent can edit via PUT /api/skills later.
4. **Write file** — `write_skill(WORKSPACE_DIR, &name, &frontmatter,
   &instructions)`.
5. **Snapshot** — `save_version(db, &name, &content, "capture", None,
   Some("captured in-session by {agent_name}"))`.
6. **Notify** — `notify(db, ui_event_tx, "skill_captured", "New skill
   captured", &format!("Agent {agent_name} captured skill: {name}"),
   json!({"skill": name, "agent": agent_name}))`. UI click navigates to
   `/skills`.
7. **Audit** — `INSERT INTO curator_decisions (skill_name, action, reason)
   VALUES (name, "captured", "in-session capture by {agent_name}")`.
8. **Return** — `"Skill '{name}' captured and active."` to the agent.

### Error handling

| Scenario | Response to agent |
| --- | --- |
| Invalid name format | `"Invalid skill name '{name}'. Use lowercase letters, digits, and hyphens only."` |
| Skill already exists | `"Skill '{name}' already exists."` |
| File write failure | `"Failed to write skill: {io_error}"` |
| DB/notify failure | Log `warn!`, continue — skill file is written |

### Agent guidance updates

**`config/skills/skill-curator.md`** — add section:

```markdown
## Capturing New Skills In-Session

Use `skill_use(action="capture")` when you notice a reusable pattern:
- A workflow you will likely need again in future sessions
- A technique that took multiple attempts to get right
- A format, style, or sequence the user explicitly prefers

Do NOT capture:
- One-off tasks specific to this session
- Trivial operations already covered by existing skills
- Skills that duplicate an existing one (use FIX instead)
```

**`scaffold/base/SOUL.md`** — add one line to the tools section:

```
- skill_use(action="capture") — create a new reusable skill from a pattern discovered this session
```

### New notification type

Add `"skill_captured"` to the notification type check in
`src/gateway/handlers/notifications.rs`. The UI bell and sound already fire for
any new notification; no frontend changes needed.

---

## Part 2 — Richer Post-Session Review

### Context expansion

`review_session_inner()` currently loads the last 30 messages and extracts user
text only (2 000 byte cap).

New context bundle (6 000 byte total cap, UTF-8 char boundary):

```
[Session metadata]
Agent: {agent_name}
Outcome: {Done|Failed|Interrupted}
Tool calls made: {comma-separated tool names, deduplicated}
Skills captured this session: {names or "none"}

[User messages]
{user message 1}
---
{user message 2}

[Assistant responses]
{assistant text 1, first 300 chars}
---
{assistant text 2, first 300 chars}
```

**Assistant text**: only the `content` field of `role == "assistant"` rows.
Tool-call JSON embedded in content is stripped (take text up to first `[{`).

**Tool names**: extracted from `role == "tool"` rows by parsing the `content`
field as JSON and reading `tool_use_id` / name. Alternatively, parse assistant
rows for `tool_calls` JSON if stored there. Deduped, sorted.

**Skills captured this session**: query `curator_decisions` for
`action = 'captured'` entries where `decided_at >= session_created_at`.
`session_created_at` is fetched via
`SELECT created_at FROM sessions WHERE id = $1` at the start of
`review_session_inner()`.

### Multiple verdicts

LLM prompt updated to allow up to 3 verdict lines:

```
Respond with 1–3 lines. Each line must be one of:
- SKIP
- FIX <skill_name>
- DERIVED <parent_skill> <new_name>
- CAPTURED <new_name>

If nothing applies, respond with a single SKIP.
```

Parser reads all non-empty lines, processes each independently, stops at 3.
A `SKIP` line is skipped; only a sole `SKIP` response means no action.

### Trigger filter update

Two independent gates (both must pass for normal sessions):

1. `tool_call_count >= min_tool_calls` (existing, configurable)
2. `user_message_count >= 2` (new — excludes single-line sessions)

**Exception:** Failed/Interrupted sessions bypass gate 1 — they are the most
informative for skill improvements.

### Timeout

Unchanged at 30 seconds. No retry on timeout (clarified in code comment).

---

## File Map

| Action | Path | Responsibility |
| --- | --- | --- |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/handlers.rs` | Add `handle_skill_capture()` function |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/tool_defs.rs` | Add `capture` action to `skill_use` tool schema |
| Modify | `crates/hydeclaw-core/src/agent/engine_dispatch.rs` | Dispatch `skill_use` capture action to new handler |
| Modify | `crates/hydeclaw-core/src/skills/evolution.rs` | Enrich `review_session_inner`, multi-verdict parse |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` | Pass session outcome + user_message_count to `spawn_skill_review` |
| Modify | `config/skills/skill-curator.md` | Add capture guidance section |
| Modify | `crates/hydeclaw-core/scaffold/base/SOUL.md` | Add `skill_use(action="capture")` to tool list |
| Modify | `crates/hydeclaw-core/src/gateway/handlers/notifications.rs` | Add `skill_captured` notification type |

---

## Testing

| Test | Assertion |
| --- | --- |
| `skill_use(capture)` valid input | File created, version saved, `curator_decisions` row inserted |
| `skill_use(capture)` duplicate name | Returns error, no file written |
| `skill_use(capture)` invalid name chars | Returns error immediately |
| `skill_use(load)` existing skill | Returns content (unchanged from current) |
| `review_session_inner` multi-verdict | All verdicts enqueued independently |
| `review_session_inner` SKIP only | Nothing enqueued |
| `review_session_inner` Failed session | Passes filter regardless of tool count |
| `review_session_inner` 1 user message | Filtered out |

---

## Out of Scope

- `skill_use(action="improve")` — edit existing skill in-session. Captured
  skills can be improved via Curator FIX or PUT /api/skills.
- Usage telemetry counters (`use_count`, `view_count`) à la Hermes. Existing
  `last_used_at` is sufficient for Phase 1 staleness detection.
- Per-agent skill directories. All captured skills go to `workspace/skills/`.
- UI changes beyond the existing bell/sound for `skill_captured` notification.
- Subagent skill capture — intentionally blocked by existing deny-list.
