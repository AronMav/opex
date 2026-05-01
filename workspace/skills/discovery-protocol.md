---
name: discovery-protocol
description: Task classification framework -- Level 0-3 decision tree with research triggers
triggers:
  - how to approach
  - where to start
  - should I research
  - task classification
  - discovery
  - discovery level
  - как подойти
  - с чего начать
  - нужно ли исследовать
  - классификация задачи
priority: 8
state: active
---

## Discovery Protocol

### Purpose

Classify every task BEFORE starting work. The classification determines how much exploration is needed before execution. Misclassification is the most common source of wasted tokens.

### Decision Tree

```
Is the pattern already in this codebase?
├── YES: Have I done this exact thing before in this project?
│   ├── YES ──> Level 0 (Execute directly)
│   └── NO: Do I know which files to modify?
│       ├── YES ──> Level 1 (Quick look, then execute)
│       └── NO ──> Level 2 (Research first)
└── NO: Do I understand the domain?
    ├── YES ──> Level 2 (Research approach)
    └── NO ──> Level 3 (Ask before acting)
```

### Level 0 -- Known Path

**Signal:** Pattern exists in codebase. Done this exact thing before. No new dependencies.

**Examples:** Add field to model, CRUD endpoint following existing patterns, fix typo, config change.

**Action:** Execute directly, verify, done.

**Time budget:** 0 minutes research.

**Anti-pattern:** Reading 5 files "just to be sure" when grep confirms the pattern.

### Level 1 -- Known Domain, Unfamiliar File

**Signal:** Know the technology, need to check 2-3 files for exact syntax or state.

**Examples:** Feature using library already in deps but in an unread file, applying pattern from one module to another.

**Action:** Read specific files (2-3 max), identify pattern, execute, verify.

**Time budget:** 2-5 minutes exploration.

**Anti-pattern:** Reading entire module when you only need one function signature.

### Level 2 -- Unknown Approach

**Signal:** Know the problem, not the solution. Multiple approaches possible. New dependencies.

**Research triggers** (any ONE means Level 2):
- New library not in deps
- External API integration
- "Choose between" in the task
- Architecture decision with long-term impact
- Unfamiliar format or protocol

**Action:** Research (docs, patterns, 2-3 options) -> Decision (with rationale) -> Plan (steps + verification) -> Execute -> Verify.

**Time budget:** 15-30 minutes research.

**Anti-pattern:** Starting to code before choosing an approach.

### Level 3 -- Unknown Domain

**Signal:** Do not understand the domain well enough to evaluate solutions.

**Research triggers** (any ONE means Level 3):
- Domain terminology you cannot define
- "Design a system for X" outside your expertise
- Multiple interacting external services
- Regulatory requirements

**Action:** Stop -> Ask clarifying questions -> Research deeply (primary docs, not tutorials) -> Propose approach for review -> Get confirmation -> Execute with extra verification.

**Time budget:** 1+ hours research. Execute only after approach confirmed.

**Anti-pattern:** Treating Level 3 as Level 2. "I will just try it" in unfamiliar domain produces plausible but wrong solutions.

### Classification Mistakes

| Mistake | Symptom | Cost |
|---------|---------|------|
| Level 0 treated as Level 2 | Reading docs for a pattern you already know | Wasted tokens, slow delivery |
| Level 2 treated as Level 0 | Jumping into code without understanding approach | Rework, wrong architecture, wasted effort |
| Level 3 treated as Level 1 | Quick skim then code in unfamiliar domain | Plausible but fundamentally wrong solution |
| Level 1 treated as Level 3 | Extensive research for a known technology | Massive token waste, analysis paralysis |

### Reclassification

**Upgrade** (0->1, 1->2, 2->3): More complexity than expected. The moment you realize "this is harder than I thought" -- reclassify upward.

**Downgrade** (3->2, 2->1): Problem simpler than feared. After initial research reveals a straightforward path -- reclassify downward.

Always document: "Reclassifying from Level X to Level Y: [reason]."

### Integration with SOUL.md

This protocol elaborates on the Discovery Classification principle in SOUL.md. SOUL.md tells you to classify (the WHAT). This skill tells you HOW to classify and WHAT TO DO at each level.
