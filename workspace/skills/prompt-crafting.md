---
name: prompt-crafting
description: Crafting effective prompts for agent tool tasks and LLM requests
triggers:
  - prompt
  - write a query
  - formulate a task
  - instruction for agent
  - промпт
  - напиши запрос
  - сформулируй задачу
  - инструкция для агента
priority: 4
---

## Effective Prompt Crafting Strategy

### When to apply

- Formulating a task for a target agent via `agent(action="ask")`
- User asks to compose a prompt or instruction
- Need to improve the quality of an LLM response

### Effective prompt structure

```
1. ROLE — who you are (expert, analyst, assistant)
2. CONTEXT — minimum needed to understand the task
3. TASK — what exactly to do
4. FORMAT — what the result should look like
5. CONSTRAINTS — what NOT to do
```

### Rules

#### Specificity
- ❌ "Analyze the data"
- ✅ "Find 3 key trends in Q1 2026 sales data, compare with Q4 2025"

#### Output format
- ❌ "Tell me about the company"
- ✅ "Give a brief overview of the company: sector, revenue, P/E, key risks — in table format"

#### One task — one prompt
- ❌ "Find information, analyze it, write a report and send it"
- ✅ Break into 4 separate steps

#### Examples in the prompt
If a specific format is needed — show an example:
```
Respond in the format:
**Conclusion**: [one sentence]
**Rationale**: [2-3 sentences]
**Confidence**: high/medium/low
```

### For agent tool tasks (agent(action="ask"))

- Provide full context — the target agent does not see the conversation history
- Specify the response format — the target agent must know what to return
- Limit the scope — "only X, do not touch Y"
- Don't send long texts — convey the essence

### Anti-patterns

- Don't add "please" and politeness — it doesn't improve LLM output
- Don't repeat the instruction 3 times "for reliability" — it wastes context
- Don't use CAPS LOCK for everything — only for critical constraints
- Don't describe how LLM works — describe what result you need
