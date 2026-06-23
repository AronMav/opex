# Skill Curator — Phase 3 Analyst/Verifier Design

**Date:** 2026-05-02  
**Status:** Approved

## Problem

Phase 3 (LlmConsolidation) gives Hyde a single agent session to both analyse and execute skill consolidation. Wrong analysis directly becomes wrong execution with no checkpoint in between. The `news-digest` skill was incorrectly archived because Hyde judged it a duplicate of `daily-briefing` based on surface similarity, without verifying capability coverage.

Prompt-level fixes (mandatory reads, ARCHIVE restrictions) reduce false positives but cannot eliminate them — they still rely on LLM reasoning being correct in one pass.

## Solution

Split Phase 3 into three steps with independent contexts:

```
Phase 3
  ├── Step A: Analyst (Hyde agent session)
  │     Reads all active skills, produces curator_proposals.json
  │
  ├── Step B: Verifier (one provider.chat() call per ARCHIVE proposal)
  │     Receives: capability_map + full content of both skills
  │     Returns: ACCEPT or REJECT with specific missing capabilities
  │
  └── Step C: Executor
        ARCHIVE (ACCEPT) → Core applies programmatically
        MERGE / FIX      → new Hyde session for content generation
        ARCHIVE (REJECT) → skipped, logged in report_md
```

**Key principle:** the Verifier receives only data (capability_map + file contents), never the Analyst's reasoning. It cannot inherit reasoning errors.

---

## Analyst

### Task

Hyde agent session. Task prompt includes:
- List of active non-pinned skills (name, description, state, last_used_at, triggers)
- Pinned skill names (never touch these)
- Instruction to write analysis to `workspace/curator_proposals.json` via `workspace_write`
- Maximum 5 proposals per run (ARCHIVE + MERGE + FIX combined; SKIP is not a proposal)
- For ARCHIVE: mandatory full read of both skills + complete capability_map with verbatim quotes

### Output — `workspace/curator_proposals.json`

```json
{
  "proposals": [
    {
      "action": "archive",
      "skill": "daily-reflection",
      "replacement": "self-improvement",
      "reason": "one sentence",
      "capability_map": [
        {
          "capability": "daily journal entry format",
          "from_quote": "exact verbatim text from daily-reflection",
          "covered_in": "self-improvement › Section 1 'Daily Reflection'",
          "covering_quote": "exact verbatim text from self-improvement covering this"
        }
      ]
    },
    {
      "action": "merge",
      "sources": ["skill-a", "skill-b"],
      "into": "skill-unified",
      "reason": "one sentence"
    },
    {
      "action": "fix",
      "skill": "research-strategy",
      "description": "what specifically to fix"
    }
  ]
}
```

**Rules enforced in prompt:**
- ARCHIVE requires `capability_map` with at least one entry per distinct capability section
- If a covering_quote cannot be found → do not add the ARCHIVE proposal at all
- Pinned skills must not appear in any proposal
- `capability_map` is not required for MERGE or FIX

---

## Verifier

One `provider.chat()` call (not an agent session) per ARCHIVE proposal.

### Input

- Full content of the skill being archived
- Full content of the replacement skill
- The `capability_map` from the Analyst's proposal

### Prompt

```
You are a skill coverage auditor. Verify that a proposed archival is safe.

Skill to archive: {skill_name}
Full content:
{archived_content}

Proposed replacement: {replacement_name}
Full content:
{replacement_content}

Capability map claimed by analyst:
{capability_map_json}

For each entry in the capability map:
1. Find the from_quote in the archived skill (exact or near-exact match required)
2. Find the covering_quote in the replacement skill (exact or near-exact match required)
3. Confirm the covering_quote addresses the same capability

Return EXACTLY one of:
  ACCEPT
  REJECT: <capability name> — <reason it is not covered>

Multiple REJECT lines are allowed (one per missing capability).
Be strict. Paraphrase is not coverage. If a quote is absent — REJECT.
```

### Response Parsing

| Response | Action |
|---|---|
| Starts with `ACCEPT` | Proposal accepted |
| Starts with `REJECT` | Proposal rejected, reason(s) logged |
| Parse error / empty | Treat as REJECT (fail safe) |

### Provider

Uses the curator's configured `provider_connection` + `model` from `[curator]` in `opex.toml`.

---

## Executor

**ARCHIVE (ACCEPT):**
1. Read skill file
2. Save version to `skill_versions` with `trigger_reason = "curator:archive:verified"`
3. Replace `state: active` → `state: archived` in frontmatter
4. Atomic write (tmp → rename)

**MERGE / FIX:**
New Hyde agent session receives only the accepted MERGE/FIX proposals and executes them via `workspace_write` / `workspace_edit`.

**ARCHIVE (REJECT):**
Skip. Add to `report_md`: `⚠ {skill} not archived — {reject_reasons}`.

**Cleanup:**
`workspace/curator_proposals.json` deleted after executor completes.

---

## Error Handling

| Situation | Action |
|---|---|
| Hyde does not create `proposals.json` | Phase 3 logs warning, exits with 0 commands; Phases 1/2 results preserved |
| `proposals.json` is invalid JSON | Same as above |
| Skill file missing between analysis and execution | Skip that proposal, warn |
| Verifier call fails / returns unparseable | Treat as REJECT |
| MERGE/FIX executor session fails | Skip that proposal, continue with remaining |

---

## Reporting

`report_md` Phase 3 section includes:
- Number of proposals generated by Analyst
- For each ARCHIVE: ACCEPTED or REJECTED with reason
- Actions applied (archives, merges, fixes)

---

## Database

No schema changes. `curator_runs.phase3` counter increments per action applied (same as before).

---

## Testing

### Unit Tests

| Test | Validates |
|---|---|
| `proposals_valid_json` | Valid JSON parsed, ARCHIVE proposals extracted |
| `proposals_invalid_json` | Malformed JSON → empty proposal list, no panic |
| `proposals_missing_file` | Missing `proposals.json` → Phase 3 skips gracefully |
| `verifier_accept` | `ACCEPT` response → proposal proceeds |
| `verifier_reject` | `REJECT: X — reason` → proposal skipped, reason captured |
| `verifier_malformed` | Unparseable response → treated as REJECT |
| `pinned_excluded` | Pinned skills absent from Analyst task summary |

### Regression Test (Pi)

Precondition: `daily-reflection` and `verification` restored to `state: active` (done). `news-digest` has `pinned: true`.

| Skill | Expected |
|---|---|
| `daily-reflection` | Proposed → full capability_map → ACCEPT → archived |
| `verification` | Proposed → full capability_map → ACCEPT → archived |
| `news-digest` | Not in proposals (pinned) |
| All others | Untouched |

### Fail-Path Test

Manually write `curator_proposals.json` with an incomplete capability_map for a non-pinned skill. Verifier must return REJECT. Skill remains `active`.

---

## Out of Scope

- Approval queue / human review before execution (may be added in v2 if needed)
- Verifier for MERGE/FIX proposals (additive changes, lower risk)
- Embedding-based similarity pre-filter (can be layered on top later)
