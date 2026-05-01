---
name: daily-reflection
description: Reflective journal — daily summaries, insights, planning the next day
triggers:
  - end of day summary
  - journal
  - what happened today
  - reflection
  - journal entry
  - log the day
  - итоги дня
  - дневник
  - что произошло сегодня
  - рефлексия
  - запиши день
priority: 4
state: active
---

## Daily Reflection Strategy

Create structured Zettelkasten entries from the day's events.

### When to use

- User asks for a daily summary
- Heartbeat at the end of the workday
- Many events have accumulated without consolidation

### Process

1. **Gather context** — `memory(action="search")` for today: what was discussed, what tasks were worked on
2. **Structure** — by category (work, projects, ideas, decisions)
3. **Extract insights** — what was learned, what was surprising, what patterns were noticed
4. **Save** — `workspace_write` to Zettelkasten

### Entry Format

```markdown
# Journal: YYYY-MM-DD

## Key Events
- [event 1] — brief description
- [event 2] — brief description

## Decisions
- [decision] — context and rationale

## Insights
- [insight] — why this matters

## Open Questions
- [question] — what needs to be clarified

## Plan for Tomorrow
- [ ] task 1
- [ ] task 2
```

### Zettelkasten Links

- Link entries to existing notes via [[links]]
- Tags: #journal #YYYY-MM-DD
- If an insight relates to a project — add a link to the project

### Principles

- Brevity — don't retell the whole day, only what's significant
- Honesty — record both mistakes and successes
- Connectivity — every entry is linked to context
- Actionability — every insight → a concrete action or question
