---
name: memory-management
description: Best practices for using memory with categories and topics
triggers:
  - запомни
  - remember
  - сохрани в память
  - save to memory
  - memory write
---

# Memory Management

## Categorizing Memories

When saving information to memory, ALWAYS specify category and topic.

### Categories
- **decision** — decisions made, choices between alternatives
- **preference** — user preferences, tastes, style
- **event** — things that happened, meetings, incidents
- **discovery** — found facts, insights, new knowledge
- **advice** — recommendations, tips, best practices
- **general** — everything else

### Topic
Free text describing the domain: project name, technology, person's name, etc.

## Examples

User: "Remember that I prefer Python for scripts"
→ memory(action="index", content="User prefers Python for scripts", category="preference", topic="programming", pinned=true)

User: "We decided to use PostgreSQL"
→ memory(action="index", content="Decision: use PostgreSQL in the project", category="decision", topic="database", pinned=true)

## Pinning

Set pinned=true for:
- Key facts about the user
- Important project decisions
- Stable preferences

pinned=false (default) for:
- Temporary information
- Conversation details
- Contextual notes

## Filtered Search

Use category and topic for precision:
- memory(action="search", query="database", category="decision") — decisions only
- memory(action="search", query="Python", topic="programming") — topic-scoped only
