---
name: self-improvement
description: Self-improvement — logging errors, conclusions, and corrections for continuous learning
triggers:
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
---

## Self-Improvement Strategy

After every significant error or correction from the user — record the conclusion in memory.

### When to log

1. **A command/operation failed unexpectedly** — remember the cause and correct approach
2. **User corrected you** — record what was wrong and what is correct
3. **Found a non-obvious solution** — save for future similar tasks
4. **Recurring pattern** — generalize into a rule

### Memory entry format

Use `memory(action="search")` to check — is there already a similar conclusion? If not, save via memory:

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

### Self-critique

After completing a complex task, ask yourself:
1. Did I complete everything that was asked?
2. Did I miss any important details?
3. Could this have been done more efficiently?
4. Is there anything worth remembering for the future?

If the answer to 3 or 4 is yes — save the conclusion to memory.

### Proactivity

- If you see a potential problem — warn before it happens
- If you know a more efficient way — suggest it
- If the task is ambiguous — clarify before starting, not after
