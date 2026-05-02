---
name: memory-management
description: Unified memory management — when and how to save information, categorize memories, and preserve conversation context across long or multi-channel sessions
triggers:
  - forgot
  - what were we talking about
  - lost context
  - remind me
  - conversation too long
  - we discussed
  - in telegram
  - in the chat
  - remember when
  - забыл
  - о чём мы говорили
  - потерял контекст
  - напомни
  - разговор слишком длинный
  - мы обсуждали
  - в телеграме
  - в чате
  - помнишь когда
  - запомни
  - remember
  - сохрани в память
  - save to memory
  - memory write
priority: 3
state: active
pinned: true
---

# Memory Management

This skill combines best practices for **when** to save information to memory, **how** to structure it with categories and topics, and **how to preserve conversation context** so you never lose track of important details.

---

## 1. When to Save to Memory

### What to save (long‑term memory)
- ✅ Conclusions and decisions (not the discussion process)
- ✅ User preferences, tastes, style
- ✅ Facts that are hard to find again
- ✅ Key project decisions and constraints

### What NOT to save
- ❌ Intermediate reasoning or raw search results
- ❌ File contents (already in workspace)
- ❌ Temporary conversation details that won’t be needed later

### Anchor points
At the start of a complex task, establish:
- The goal of the conversation
- Key constraints
- Decisions already made

### Progressive summarization
When a conversation becomes long:
- Summarize intermediate results
- Save conclusions to memory (`memory(action="index")`)
- Reference the saved version instead of repeating the whole discussion

---

## 2. How to Save: Categories and Topics

Always specify **category** and **topic** when saving. This makes future searches precise.

### Categories
- **decision** — decisions made, choices between alternatives
- **preference** — user preferences, tastes, style
- **event** — things that happened, meetings, incidents
- **discovery** — found facts, insights, new knowledge
- **advice** — recommendations, tips, best practices
- **general** — everything else

### Topic
Free text describing the domain: project name, technology, person’s name, etc.

### Examples
User: "Remember that I prefer Python for scripts"
→ memory(action="index", content="User prefers Python for scripts", category="preference", topic="programming", pinned=true)

User: "We decided to use PostgreSQL"
→ memory(action="index", content="Decision: use PostgreSQL in the project", category="decision", topic="database", pinned=true)

---

## 3. Pinning and Filtered Search

### Pinning
- **pinned=true** for: key facts about the user, important project decisions, stable preferences
- **pinned=false** (default) for: temporary information, conversation details, contextual notes

### Filtered search
Use category and topic to narrow results:
- `memory(action="search", query="database", category="decision")` — decisions only
- `memory(action="search", query="Python", topic="programming")` — topic‑scoped only

---

## 4. Conversation Context Management

### Topic switching
When the user changes topics:
- Wrap up the current one (brief summary if needed)
- Do not carry context from the previous topic into the new one
- If topics are related, state the connection explicitly

### Signs of context loss
- Repeating what was already said
- Contradicting yourself
- User says “we discussed this” / “I already said”
- Confusing details from different topics

**When detected** → `memory(action="search")` for relevant context.

### Cross‑channel context
Sessions are shared across channels (Telegram, UI, etc.). If the user references a previous conversation:
- Use `memory(action="search")` for long‑term facts
- Use `session(action="history")` for recent messages
- Different channels have different formats (Telegram: MarkdownV2, UI: markdown)
- Files/images are bound to a channel — do not forward between channels
- Do not mention technical channel details to the user

---

## 5. Principles

- Better to ask than to guess
- A brief summary at the end of a complex discussion is free insurance
- If unsure about context — acknowledge it and clarify
- Always save decisions and preferences with clear categories and topics
