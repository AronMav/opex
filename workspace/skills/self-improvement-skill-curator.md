---
name: self-improvement-skill-curator
description: Specialized self-improvement loop for skill curation — capture insights, errors, and corrections during skill creation and repair workflows.
triggers:
  - skill repair
  - skill creation
  - skill curation
  - curate skills
  - repair queue
  - skill fix
  - исправить навык
  - курировать навыки
  - ремонт навыка
priority: 6
state: active
parent: self-improvement
---

# Self-Improvement: Skill Curator Specialization

Specialized variant of `self-improvement` tailored for skill curation workflows.

## When to use

- Processing a skill repair (fix, derived, or captured)
- Creating a new skill from scratch
- Reviewing skill lifecycle (stale/archive transitions)
- After a repair succeeds or fails — capture the lesson

## Curation-Specific Lessons

After each repair operation, evaluate:

1. **Was the diagnosis accurate?** — If not, note why for future pattern matching.
2. **Did the fix work on first attempt?** — If retries were needed, capture the correct approach.
3. **Could the repair have been automated?** — If yes, suggest a rule or script.

## Error Patterns in Skill Curation

| Symptom | Likely Cause | Correct Approach |
|---------|-------------|-----------------|
| Fix repair fails — skill unchanged | diagnosis too vague | Re-read skill, identify exact broken lines, apply targeted edit |
| Fix repair fails — diagnosis is «FIX {name}» with no specifics | Diagnosis was auto-generated as a placeholder, not a real diagnosis | **Refuse to apply blind fix.** Instead: read skill, compare to parent/procedure, identify actual gap, update diagnosis via PATCH, then fix — OR if no gap found, mark failed |
| Derived skill rejected | wrong parent or too broad triggers | Narrow triggers to the specialization domain |
| Captured skill has empty body | insufficient context in diagnosis | Re-read task history, extract concrete steps |

### Lesson from repairs aaadabea, a705a39e (2026-05-17)

A diagnosis that merely repeats the skill name (e.g. «FIX self-improvement-skill-curator») is a **placeholder**, not a diagnosis. Applying a fix based on such a diagnosis is guaranteed to fail — there is no actual symptom to address. The correct approach is to read the skill, compare it against its parent/procedure, identify the real gap, and only then apply a targeted edit. If no gap is found, mark the repair as `failed` with resolution «No actionable diagnosis provided; skill structure appears correct.»

## Memory Format

```
[LESSON] skill_curation: brief description
Context: repair ID, skill name, kind
Error/Failure: what went wrong
Correct approach: what fixed it
Priority: medium | high
```

## Self-Critique for Curation

After each repair batch:

1. How many repairs were processed?
2. How many succeeded on first attempt?
3. What patterns emerged across failures?
4. Is there a rule worth extracting into this skill?

Save significant findings to memory using the format above.
