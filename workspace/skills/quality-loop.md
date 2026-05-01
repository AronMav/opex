---
name: quality-loop
description: Research-Plan-Execute-Verify workflow with explicit phase gates
triggers:
  - workflow
  - quality process
  - how to structure work
  - phase gates
  - рабочий процесс
  - качество работы
  - как структурировать
priority: 7
state: active
---

## Quality Loop

### Purpose

Structure every non-trivial task as a four-phase loop: Research, Plan, Execute, Verify. Each phase has an explicit gate -- you must satisfy the gate before proceeding. Skipping phases is the primary cause of rework.

### When to Use This

- **Use full loop:** Level 2-3 tasks (unknown approach or domain), multi-file changes, architectural decisions
- **Use abbreviated loop** (Plan-Execute-Verify): Level 1 tasks -- skip Research when approach is known
- **Skip entirely:** Level 0 tasks -- just execute and verify

### Phase 1: Research

**Purpose:** Understand the problem space before committing to a solution.

**Activities:**
- Read relevant source files and documentation
- Identify existing patterns
- List constraints (performance, compatibility, token budget)
- Evaluate 2-3 approaches if path is not obvious

**Output:**
- Problem statement (1-2 sentences)
- Constraints identified
- Approach chosen with rationale

**Gate:** Can you explain the approach to someone else in 2-3 sentences? YES = proceed to Plan. NO = continue researching.

### Phase 2: Plan

**Purpose:** Define exactly what to do, in what order, and how to verify each step.

**Activities:**
- List concrete steps (not vague intentions)
- For each step: what file, what change, what the result looks like
- Define verification criteria: "What must be TRUE when this step is done?"
- Identify dependencies
- Estimate scope: if more than 5 files or 3 distinct concerns, split

**Output:**
- Ordered list of steps with file paths
- Verification criterion per step
- Dependencies between steps

**Gate:** Does every step have a verification criterion? YES = proceed to Execute. NO = add criteria.

**Goal-backward check:** Start from the end state. "What must be TRUE when the whole task is done?" Work backward to ensure every step contributes to at least one truth.

### Phase 3: Execute

**Purpose:** Implement the plan. Follow it, do not improvise.

**Rules:**
- Execute in dependency order
- After each step, verify immediately -- do not batch
- If step fails: stop, diagnose, fix before proceeding
- If plan is wrong: go back to Plan, do not patch
- Track changes: file paths, line ranges, what changed

**Output:**
- Changes made
- Per-step verification results

**Gate:** Did every step pass its verification? YES = proceed to Verify. NO = fix failures first.

### Phase 4: Verify

**Purpose:** Confirm the entire task is complete, not just individual steps.

**Activities:**
- Re-read original task/requirement
- Check each success criterion against actual state
- Run integration-level checks
- Look for side effects
- If verification skill is loaded, use full adversarial protocol

**Checks:**
- All success criteria pass
- No regressions
- Changes are minimal
- Code/content follows conventions

**Gate:** Can you demonstrate completion with evidence? YES = done, report with evidence. NO = identify missing, return to Execute.

### Loop Back

If Verify fails:
- **Minor issue** (1-2 criteria) -> return to Execute
- **Major issue** (wrong approach) -> return to Plan
- **Fundamental issue** (misunderstood problem) -> return to Research

After 2 loop-backs at same phase, escalate.

### Phase Discipline

| Violation | What Happens | Fix |
|-----------|-------------|-----|
| Skip Research | Wrong approach chosen, rework later | Always classify task level first |
| Skip Plan | Ad-hoc changes, missed steps, inconsistencies | Write steps before first edit |
| Skip per-step verification | Errors compound, harder to diagnose | Verify after every single step |
| Skip Verify | "Done" but actually broken | Never declare done without evidence |
| Patch instead of replan | Increasingly fragile solution | If plan was wrong, rewrite the plan |

### Abbreviated Workflows

**For Level 1 tasks:** Plan (2-3 bullets + criteria) -> Execute -> Verify. Skip Research.

**For urgent fixes:** Execute -> Verify. Skip Research and Plan. NEVER skip Verify.

### Integration

This workflow connects with:
- **Discovery Classification** (SOUL.md) determines which variant to use
- **Verification Protocol** (`skill_use("verification")`) for Phase 4 deep verification
- **Goal-Backward Reasoning** (SOUL.md) drives the planning phase
- **Error Recovery** (SOUL.md) handles Execute phase failures
