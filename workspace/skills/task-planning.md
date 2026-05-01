---
name: task-planning
description: Complex task planning — decomposition, progress tracking, file-based plans
triggers:
  - plan this
  - make a plan
  - break into steps
  - complex task
  - project
  - roadmap
  - спланируй
  - составь план
  - разбей на шаги
  - сложная задача
  - проект
  - дорожная карта
priority: 8
state: active
---

## Task Planning Strategy

For any task with 3+ steps — create a structured plan before executing.

### When to plan

- Task requires more than 3 steps
- There are dependencies between steps
- Multiple tools need to be coordinated
- User asks to "plan" or "break into steps"

### Plan format

Create a plan in workspace via `workspace_write`:

```markdown
# Plan: [task name]

## Goal
Clear statement of the final result.

## Steps
- [ ] Step 1: description
- [ ] Step 2: description (depends on: step 1)
- [ ] Step 3: description
...

## Notes
- Risks and constraints
- Open questions
```

### Decomposition Principles

1. **Atomicity** — each step must be completable in a single action
2. **Dependencies** — explicitly state what depends on what
3. **Verifiability** — for each step, it's clear when it is "done"
4. **Priorities** — critical path first, secondary tasks after

### Updating Progress

As work proceeds, update the plan:
- `[ ]` → `[x]` for completed steps
- Add notes about problems found
- Adjust remaining steps if context has changed

### Report to User

After completing the plan — brief summary:
- What was done
- What failed (and why)
- Next steps (if any)

### Task Sizing

Target each task at 15-60 minutes of execution time.

| Duration | Action |
|----------|--------|
| Less than 15 min | Too small -- combine with a related task |
| 15-60 min | Right size |
| More than 60 min | Too large -- split along concern boundaries |

**Too-large signals:**
- Touches more than 5 files
- Multiple distinct concerns (DB schema + API handler + UI component)
- Action description needs more than one paragraph
- Contains both "create X" and "wire X into Y"

**Too-small signals:**
- One task sets up state for the next
- Separate tasks touch the same file
- Neither task is meaningful alone

### Split Signals

When ANY of these are true, split the task:

1. **Multiple subsystems** -- DB + API + UI in one task = three tasks
2. **More than 5 file modifications** -- scope is too broad
3. **Mixed creation and integration** -- create the component, then wire it in
4. **Checkpoint + implementation** -- do the work, then verify separately
5. **Discovery + implementation** -- research the approach, then execute it

### Vertical Slices

Prefer vertical slices (full feature) over horizontal layers (all models, then all APIs):

**Vertical (PREFER):**
```
Task 1: User feature (model + API + UI)
Task 2: Product feature (model + API + UI)
```
Result: Both tasks can run independently.

**Horizontal (AVOID):**
```
Task 1: Create User model, Product model
Task 2: Create User API, Product API
Task 3: Create User UI, Product UI
```
Result: Fully sequential -- Task 2 needs Task 1, Task 3 needs Task 2.

**When horizontal is necessary:**
- Shared foundation required (auth before protected routes)
- Genuine type dependencies across features
- Infrastructure setup (database, config) before feature work

### Dependency-First Thinking

Before ordering tasks, think dependencies -- not sequence:

1. What does this task NEED? (files, types, APIs that must exist)
2. What does this task CREATE? (files, types, APIs others need)
3. Can it run independently? (no dependencies = do first or in parallel)

Group tasks into waves:
- **Wave 1:** No dependencies (roots)
- **Wave 2:** Depends only on Wave 1
- **Wave 3:** Depends on Wave 2

Maximize tasks per wave to minimize total execution time.
