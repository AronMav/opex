---
name: architecture-design
description: System architecture design — patterns, trade-offs, decision documentation
triggers:
  - architecture
  - design the system
  - how to organize
  - microservices
  - project structure
  - design
  - system components
  - архитектура
  - спроектируй систему
  - как организовать
  - структура проекта
  - дизайн системы
priority: 6
state: active
---

## Architecture Design Strategy

### Architectural Decision Process

#### 1. Understand the context
- What problem are we solving?
- What are the constraints (budget, time, team, hardware)?
- What qualities are critical (performance, reliability, scalability)?
- What is NOT critical (trade-offs)?

#### 2. Solution options
Always consider at least 2-3 options:
- **Option A**: description, pros, cons
- **Option B**: description, pros, cons
- **Recommendation**: which one and why

#### 3. Documenting the decision (ADR)
```
## Decision: [name]
**Status**: accepted / under discussion / rejected
**Context**: why the question arose
**Decision**: what was decided
**Consequences**: what this means for the project
**Alternatives**: what was considered and why rejected
```

### Key Principles

- **Simplicity** — start with the simplest solution, add complexity only when necessary
- **Separation of concerns** — each component does one thing well
- **Loose coupling** — components communicate through clear interfaces
- **Observability** — logs, metrics, health checks from the start
- **Incrementality** — can it be migrated gradually?

### Common Trade-offs

| Choice | For | Against |
|--------|-----|---------|
| Monolith vs microservices | Simplicity vs scalability |
| SQL vs NoSQL | Consistency vs flexibility |
| Sync vs async | Simplicity vs performance |
| Cache vs query | Speed vs freshness |
| Own code vs library | Control vs development speed |

### Dependency Analysis

Before designing components, map what depends on what:

1. **For each component, record:**
   - `needs` -- what must exist before this can be built
   - `creates` -- what this produces that others consume
   - `has_external` -- requires external service or API

2. **Build the dependency graph:**
   - Components with no `needs` are roots (build first)
   - Components that only `need` roots can run in parallel (Wave 2)
   - Shared dependencies force sequencing

3. **Prefer vertical slices over horizontal layers:**
   - Vertical: Feature A (model + API + UI), Feature B (model + API + UI) -- parallel
   - Horizontal: All models, then all APIs, then all UI -- sequential bottleneck
   - Use horizontal only when shared foundation is genuinely required (auth before protected features)

4. **File ownership:** If two components modify the same file, they cannot run in parallel. Restructure to eliminate overlap when possible.

### Interface Extraction

Before implementation, extract the contracts that connect components:

- **Types and interfaces** -- data shapes that cross boundaries
- **Function signatures** -- public API of each module
- **API contracts** -- request/response shapes for endpoints
- **Event schemas** -- structure of emitted events

Capture these in a dedicated types file or interface block. Implementation code imports from these contracts -- never the reverse.

**When to extract:**
- Plan touches files that import from other modules
- Plan creates a new API endpoint (extract request/response types)
- Plan modifies a component (extract its props interface)
- Plan depends on a previous step's output

**When to skip:**
- Self-contained work (creates everything from scratch)
- Pure configuration (no code interfaces)
- Level 0 tasks with established patterns

### Scope Estimation

Estimate work size to avoid overcommitting:

| Files Modified | Scope |
|----------------|-------|
| 1-3 files | Small -- single task |
| 4-6 files | Medium -- may need 2 tasks |
| 7+ files | Large -- split into separate concerns |

**Split signals (any ONE means split):**
- More than 5 file modifications
- Multiple distinct subsystems (DB + API + UI)
- Task description needs more than one paragraph
- Contains both creation and integration work

**Estimation tip:** If scope is uncertain, start with `skill_use("quality-loop")` Research phase to bound the work before committing to a plan.
