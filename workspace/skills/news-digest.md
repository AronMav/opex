---
name: news-digest
description: News collection and digest — aggregation from multiple sources, filtering, prioritization
triggers:
  - news
  - digest
  - what's new
  - news roundup
  - latest news
  - новости
  - дайджест
  - что нового
  - обзор новостей
  - последние новости
priority: 6
tools_required:
  - search_web
state: active
---

## News Collection Strategy

### Process

#### 1. Define the focus
- Topic: technology / finance / world / specific industry?
- Period: today / this week / this month?
- Language: ru / en / both?

#### 2. Gather from multiple sources
Make 3-5 search queries with different phrasings:
- `search_web` — primary source (fresh results)
- Vary queries: general → specific → alternative phrasings
- For finance: add names of key indexes/companies
- For technology: add names of technologies/companies

#### 3. Filter and rank
Rate each item by:
- **Impact**: how much does this affect the user?
- **Credibility**: what source is it from?
- **Freshness**: how recent is the information?
- **Uniqueness**: is it a duplicate of another story?

#### 4. Digest format

```
## Digest: [topic] — [date]

### Top Stories
1. **[Headline]** — brief description (1-2 sentences)
2. **[Headline]** — brief description

### Notable
3. **[Headline]** — description
4. **[Headline]** — description

### Also
- [brief item]
- [brief item]

### Trends
- [pattern observation]
```

### For automated digests (cron)

- Do not repeat news from the previous digest — `memory(action="search")` for the last one
- If nothing significant — HEARTBEAT_OK
- Maximum 5-7 items, don't overload
- Flag only genuinely significant events
