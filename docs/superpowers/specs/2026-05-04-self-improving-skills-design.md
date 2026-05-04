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

1. Give agents a `skill(action="capture")` tool to create skills immediately
   during a session, with notification and Curator traceability.
2. Keep `skill(action="load")` as part of the same tool (replacing the current
   separate `skill_use` handler).
3. Enrich `review_session_inner()` to include assistant responses, tool call
   names, session outcome, and allow up to 3 verdicts per session.

---

## Part 1 — In-Session Skill Capture

### Tool definition

New system tool `skill` with two actions, replacing the existing `skill_use`:

| Action | Description |
| --- | --- |
| `load` | Load a skill's instructions into context (current `skill_use` behaviour) |
| `capture` | Create a new skill file immediately from session learning |

The tool is a system tool (hardcoded, not YAML). It is registered alongside
`workspace_write`, `memory_write`, etc. in the engine's tool dispatch.

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
   return error: `"Skill '{name}' already exists. Use skill(action='load') to
   read it, or propose a different name."`.
3. **Build frontmatter** — `SkillFrontmatter { name, description,
   triggers: split(","), tools_required: split(","), priority: 0,
   state: Active, pinned: None, last_used_at: None }`.
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
| DB notification failure | Log `warn!`, continue — skill file is written |

### Agent guidance updates

**`config/skills/skill-curator.md`** — add section:

```markdown
## Capturing New Skills In-Session

Use `skill(action="capture")` when you notice a reusable pattern:
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
- skill(action="capture") — create a new reusable skill from a pattern discovered this session
```

### New notification type

Add `skill_captured` to the notification type enum/check in
`src/gateway/handlers/notifications.rs`. The UI bell and sound already fire for
any new notification; no frontend changes needed for basic notification.

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

Assistant messages are text-only: tool_call JSON blocks are stripped. Only the
`content` field of `role == "assistant"` rows is included.

Tool names come from parsing `role == "tool"` rows: the `tool_call_id` links
back to the assistant's `tool_calls` list to extract `function.name`.

Skills captured in-session come from querying `curator_decisions` for
`action = "captured"` entries created after `session.created_at`.

### Multiple verdicts

LLM prompt is updated to allow up to 3 verdict lines:

```
Respond with 1–3 lines. Each line must be one of:
- SKIP
- FIX <skill_name>
- DERIVED <parent_skill> <new_name>
- CAPTURED <new_name>

If nothing applies, respond with a single SKIP.
```

Parser reads all non-empty lines, processes each independently, stops at 3.
`SKIP` on any line is ignored (only blocks if it is the only line).

### Trigger filter update

Two independent gates (both must pass):

1. `tool_call_count >= min_tool_calls` (existing, configurable)
2. `user_message_count >= 2` (new — prevents single-line sessions)

Failed sessions (`Interrupted` or `Failed` outcome) pass the filter regardless
of tool count: they are the most informative for skill improvements.

### Timeout

Unchanged at 30 seconds. No retry on timeout (already the case — clarified in
comment).

---

## File Map

| Action | Path | Responsibility |
| --- | --- | --- |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/handlers.rs` | Add `handle_skill_capture()` function |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/tool_defs.rs` | Add `capture` action to `skill_use` tool schema |
| Modify | `crates/hydeclaw-core/src/agent/engine_dispatch.rs` | Dispatch `skill_use` capture action to new handler |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/dispatch.rs` | No change needed — `skill_use` already in allowlist |
| Modify | `crates/hydeclaw-core/src/skills/evolution.rs` | Enrich `review_session_inner`, multi-verdict parse |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` | Pass session outcome + user_message_count to `spawn_skill_review` |
| Modify | `config/skills/skill-curator.md` | Add capture guidance section |
| Modify | `crates/hydeclaw-core/scaffold/base/SOUL.md` | Add `skill_use(action="capture")` to tool list |
| Modify | `crates/hydeclaw-core/src/gateway/handlers/notifications.rs` | Add `skill_captured` notification type |

---

## Testing

| Test | Assertion |
| --- | --- |
| `skill(capture)` valid input | File created, version saved, `curator_decisions` row inserted |
| `skill(capture)` duplicate name | Returns error, no file written |
| `skill(capture)` invalid name chars | Returns error immediately |
| `skill(load)` existing skill | Returns content (same as current `skill_use`) |
| `review_session_inner` multi-verdict | All 3 verdicts enqueued independently |
| `review_session_inner` SKIP only | Nothing enqueued |
| `review_session_inner` Failed session | Passes filter regardless of tool count |
| `review_session_inner` 1 user message | Filtered out (< 2 user messages) |

---

## Out of Scope

- `skill(action="improve")` — edit existing skill in-session. Can be added
  later; `capture` + Curator FIX covers the need for now.
- Usage telemetry counters (`use_count`, `view_count`) à la Hermes. The
  existing `last_used_at` is sufficient for Phase 1 staleness detection.
- Per-agent skill directories. All captured skills go to `workspace/skills/`.
- UI changes for `skill_captured` notification beyond the existing bell/sound.
