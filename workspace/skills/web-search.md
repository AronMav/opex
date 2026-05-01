---
name: web-search
description: Web search strategy — when to use the primary search engine and when to use Brave
triggers:
  - search
  - find online
  - google
  - look it up
  - search the web
  - поиск
  - найди в интернете
  - загугли
  - поищи
  - найди информацию
tools_required:
  - search_web
  - search_web_fresh
  - duckduckgo_search
  - tavily_search
priority: 10
state: active
---

## Search Provider Selection

| Need | Tool | Why |
|------|------|-----|
| General web search | `search_web` (SearXNG) | Primary, aggregates multiple engines |
| Fresh news / broader index | `search_web_fresh` (Brave) | When SearXNG misses recent content |
| Quick factual answer | `duckduckgo_search` | Instant answers, definitions, no API key |
| Deep page content extraction | `tavily_search` | Returns clean page text for analysis |

## Strategy

Start with `search_web` — fast private search engine.

Switch to `search_web_fresh` if:
- search_web returned few results or irrelevant ones
- You need fresh news (Brave indexes more frequently)
- The query is technical or financial and needs broad indexing

Use `duckduckgo_search` when:
- You need a quick factual answer, definition, or summary
- No API key is available for other providers

Use `tavily_search` when:
- You need full extracted page content for deep analysis
- Research tasks requiring clean text, not just links

Typical flow:
1. search_web(q="query", language="ru")
2. If ≥5 good results — sufficient
3. If not — search_web_fresh(q="query") to supplement
4. For quick facts — duckduckgo_search(q="query")
5. For deep content extraction — tavily_search(query="query")
