---
name: self-improvement
description: Continuous self-improvement through daily reflection and systematic error logging — capture insights, lessons, and corrections to grow over time.
triggers:
  # Daily reflection triggers
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
  # Error logging triggers
  - error
  - not working
  - wrong
  - fix this
  - remember this
  - keep in mind for future
  - ошибка
  - не работает
  - неправильно
  - исправь
  - запомни это
  - учти на будущее
priority: 5
state: active
pinned: true
---

## Self-Improvement Strategy

This skill merges **daily reflection** and **error logging** into a single continuous improvement loop.  
Use it to record what happened, extract lessons, and systematically remember corrections and insights.

### When to use

- End of day / heartbeat — consolidate the day’s events into a structured journal entry.
- After any significant error, correction, or non-obvious solution — log the lesson.
- When the user explicitly asks to reflect, journal, or remember something for the future.
- When a recurring pattern emerges — generalise it into a rule.

---

## 1. Daily Reflection

### Process

1. **Gather context** — `memory(action="search")` for today: discussions, tasks, decisions.
2. **Structure** — group by category (work, projects, ideas, decisions).
3. **Extract insights** — what was learned, what was surprising, what patterns were noticed.
4. **Save** — `workspace_write` to the Zettelkasten.

### Journal Entry Format

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

- Link entries to existing notes via `[[links]]`.
- Tags: `#journal #YYYY-MM-DD`.
- If an insight relates to a project, add a link to that project note.

---

## 2. Error & Lesson Logging

### When to log

1. **A command/operation failed unexpectedly** — remember the cause and correct approach.
2. **User corrected you** — record what was wrong and what is correct.
3. **Found a non-obvious solution** — save for future similar tasks.
4. **Recurring pattern** — generalise into a rule.

### Memory Entry Format

Use `memory(action="search")` to check for existing similar conclusions. If none, save via `memory`:

```
[LESSON] category: brief description
Context: what happened
Correct approach: how to handle it in the future
Priority: low | medium | high
```

### Categories

- **tool_usage** — correct use of tools
- **user_preference** — user preferences
- **domain_knowledge** — domain-specific knowledge
- **error_pattern** — typical errors and their solutions
- **workflow** — effective work processes

---

## 3. Self-Critique

After completing a complex task, ask yourself:

1. Did I complete everything that was asked?
2. Did I miss any important details?
3. Could this have been done more efficiently?
4. Is there anything worth remembering for the future?

If the answer to 3 or 4 is **yes** — save the conclusion to memory using the lesson format above.

---

## 4. Proactivity

- If you see a potential problem — warn before it happens.
- If you know a more efficient way — suggest it.
- If the task is ambiguous — clarify before starting, not after.

---

## 5. Principles (for both reflection and logging)

- **Brevity** — record only what is significant; don’t retell the whole day.
- **Honesty** — capture both mistakes and successes.
- **Connectivity** — every entry is linked to context (Zettelkasten links, memory tags).
- **Actionability** — every insight should lead to a concrete action, question, or rule.
- **Continuity** — treat reflection and error logging as a single habit; they feed each other.
