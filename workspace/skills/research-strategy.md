---
name: research-strategy
description: Deep topic research — gathering, analyzing, and synthesizing information from multiple sources
triggers:
  - research
  - analyze
  - analyze in depth
  - deep research
  - find and analyze
  - study the topic
  - gather information
  - overview
  - give me analytics
  - figure out
  - исследование
  - глубокий анализ
  - глубокое исследование
  - найди и проанализируй
  - анализ
  - изучи тему
  - собери информацию
  - обзор
  - дай аналитику
  - разберись
priority: 7
state: active
---

## Deep Research Strategy

For questions requiring information from multiple sources.

### Process

#### 1. Define the scope
- What exactly needs to be found?
- What depth is needed (overview vs detailed analysis)?
- What language to search in (ru/en)?
- Are there time/freshness constraints?

#### 2. Gather information
- Start with `search_web` for the overall picture
- Use `search_web_fresh` for current data and news
- Check `memory(action="search")` — you may have researched this topic before
- Make 3-5 different queries with varying phrasings

#### 3. Cross-verification
- Don't trust a single source
- If data is contradictory — note it explicitly
- State the confidence level: confirmed / probable / unconfirmed

#### 4. Synthesis
- Structure findings by topic, not by source
- Highlight key conclusions
- Note gaps — what could NOT be found
- Add confidence assessment per finding: confirmed / probable / unconfirmed

#### 5. Saving
- Valuable facts → `memory(action="search")` (for future retrieval)
- Detailed analysis → `workspace_write` (file in workspace)

### Report Format

```
## Topic: [name]

### Key Conclusions
1. ...
2. ...

### Details
[by section]

### Sources
- [brief description] — where it came from

### Gaps
- What could not be determined
```

### Rules

- Never present single-source information as fact
- Always note when sources disagree
- Prefer recent sources over old ones
- Use `search_web_fresh` for time-sensitive topics
- Save key findings to `memory(action="index")` for future reference
- Do not generate facts — if not found, say so
- Keep search queries short and precise (3-5 words)
- For Russian-language topics, search in both Russian and English

### Discovery Levels Integration

Research depth should match the task's discovery level. See `skill_use("discovery-protocol")` for the full classification framework.

| Discovery Level | Research Action |
|-----------------|-----------------|
| Level 0 (Known Path) | No research needed. Execute directly. |
| Level 1 (Known Domain) | Quick look: read 2-3 files, confirm syntax. 2-5 minutes. |
| Level 2 (Unknown Approach) | Standard research: evaluate 2-3 options, check docs, decide approach. 15-30 minutes. Use the full Process described above. |
| Level 3 (Unknown Domain) | Deep research: primary documentation, domain expert sources, propose approach for review before executing. 1+ hours. |

**Before starting any research, classify the task level first.** The most common research mistake is Level 0 tasks getting Level 2 treatment (wasted tokens reading docs for known patterns) or Level 2 tasks getting Level 0 treatment (jumping into code without understanding the approach).

### Mandatory Discovery Protocol

Certain task signals MUST trigger research before execution:

**Level 2 triggers (any ONE = research required):**
- New library not in current dependencies
- External API integration
- "Choose between" or "evaluate" in the task description
- Architecture decision with long-term impact
- Unfamiliar file format or protocol

**Level 3 triggers (any ONE = deep research required):**
- Domain terminology you cannot define precisely
- "Design a system for X" in an unfamiliar domain
- Multiple interacting external services
- Regulatory or compliance requirements
- Niche domains: 3D, games, audio, shaders, ML

**Research output requirements by level:**

| Level | Output |
|-------|--------|
| Level 1 | Mental model confirmed, proceed |
| Level 2 | Problem statement + constraints + chosen approach with rationale |
| Level 3 | Problem statement + constraints + 2-3 evaluated options + chosen approach + confirmation before executing |

**Reclassification:** If during research you discover the task is simpler or harder than classified, adjust the level and document: "Reclassifying from Level X to Level Y: [reason]." See `skill_use("discovery-protocol")` for reclassification rules.
