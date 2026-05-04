---
name: skill-curator
description: Curate, repair, and archive agent skills based on usage and evolution signals
triggers:
  - skill repair
  - skill maintenance
  - curate skills
  - review skills
  - archive stale skills
  - pending repairs
  - skill lifecycle
tools_required:
  - workspace_edit
  - workspace_read
priority: 8
---

## Skill Curation Procedure

### 1. Process Repair Queue

Call `GET /api/skills/repairs?status=pending`. For each pending item:

**kind=fix:**
1. Read skill: `workspace_read workspace/skills/{skill_name}.md`
2. Understand `diagnosis` — what is wrong
3. Apply targeted fix via `workspace_edit` (preserve frontmatter, fix instructions body)
4. Confirm fix: `PATCH /api/skills/repairs/{id}` with `{"status":"done","resolution_note":"<what was fixed>"}`
5. On failure: `PATCH /api/skills/repairs/{id}` with `{"status":"failed","resolution_note":"<why failed>"}`

**kind=derived:**
1. Read parent skill to understand the pattern
2. Create specialized variant: `workspace_write workspace/skills/{parent}-{specialization}.md`
3. Use the same frontmatter structure as parent, narrowed triggers and description
4. Mark done: `PATCH /api/skills/repairs/{id}` with `{"status":"done","resolution_note":"created {new_skill_name}"}`

**kind=captured:**
1. The `diagnosis` field contains the detected pattern description
2. Create new skill: `workspace_write workspace/skills/{name}.md` with appropriate frontmatter
3. Mark done with the created skill name

### 2. Lifecycle Review (weekly, triggered by CRON)

Get skill list: `GET /api/skills`

For each skill where `state` or `last_used_at` is missing:
- Add missing fields via `workspace_edit`: set `state: active`, `last_used_at: null`

For each skill with `last_used_at` present:
- Calculate age: current date minus `last_used_at`
- Age > 30 days AND `state: active` → set `state: stale` via `workspace_edit`
- Age > 90 days AND `state: stale` → move to `workspace/skills/archived/{name}.md` AND set `state: archived`

**Never delete skills — only archive. Archived skills are recoverable by moving back.**

**Pinned skills** (`priority >= 10`) are exempt from all lifecycle transitions.

---

## Capturing New Skills In-Session

Use `skill_use(action="capture")` when you notice a reusable pattern:
- A workflow you will likely need again in future sessions
- A technique that took multiple attempts to get right
- A format, style, or sequence the user explicitly prefers

**Do NOT capture:**
- One-off tasks specific to this session only
- Trivial operations already covered by an existing skill
- Patterns that duplicate an existing skill (use FIX instead)

**Example:**

    skill_use(action="capture",
      name="image-resize-for-telegram",
      description="Resize images to ≤10MB before sending via Telegram",
      triggers="resize image, compress image, telegram image",
      tools_required="code_exec",
      instructions="## Steps\n1. Read image size\n2. ...")
