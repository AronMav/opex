---
name: news-digest-pattern
description: >
  Pattern and trend detection from news — identifies recurring themes, emerging trends,
  and cyclical patterns across news cycles over time. Compares current news against
  historical context to surface non-obvious shifts.
triggers:
  - news patterns
  - news trends
  - recurring themes
  - what trends
  - news analysis
  - паттерны новостей
  - тренды в новостях
  - повторяющиеся темы
  - анализ новостей
  - что меняется
priority: 3
tools_required:
  - search_web
  - memory
state: active
---

## News Pattern Detection Strategy

### Process

#### 1. Gather recent news
- 3-5 `search_web` queries on the topic (varied phrasings)
- Focus on the last 3-7 days

#### 2. Retrieve historical context
- `memory(action="search", query="<topic> news digest", limit=10)` — find past digests and notes
- Compare current headlines with historical ones

#### 3. Identify patterns
Look for:
- **Emerging themes**: topics that weren't covered before but now appear repeatedly
- **Escalation/De-escalation**: tone shifts (crisis → resolution, optimism → concern)
- **Cyclical patterns**: seasonal, quarterly, or event-driven recurrences
- **Absence of expected news**: something usually covered but now missing

#### 4. Output format

```
## Pattern Analysis: [topic] — [date]

### Emerging Trends
1. **[Pattern]** — evidence and significance
2. **[Pattern]** — evidence and significance

### Shifts from Previous Cycle
- [What changed vs. last known state]

### Cyclical Signals
- [Recurring pattern and expected timeline]

### Gaps
- [Expected coverage that's absent — why?]

### Confidence Assessment
- High: [patterns with strong evidence]
- Medium: [patterns with limited data]
- Speculative: [early signals needing more data]
```

### Rules
- Always cite sources for each identified pattern
- Distinguish between correlation and causation
- Flag when a "pattern" is based on fewer than 3 data points as speculative
- If no meaningful patterns detected: state this explicitly, don't force analysis
