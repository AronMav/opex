---
name: web-search
description: Web search strategy — single search_web entry point backed by configurable providers (SearXNG / Ollama Cloud / Brave)
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
  - duckduckgo_search
  - tavily_search
priority: 10
state: active
last_used_at: "2026-05-02T16:00:00.201292506+00:00"
---

## Search Provider Selection

| Need | Tool | Why |
|------|------|-----|
| General web search | `search_web` | Primary entry point — routed to the highest-priority active provider (SearXNG, Ollama Cloud, Brave, etc.) |
| Quick factual answer | `duckduckgo_search` | Instant answers, definitions, no API key |
| Deep page content extraction | `tavily_search` | Returns clean page text for analysis |

## Strategy

Use `search_web` as the single web-search entry point. The active provider is configured in the Providers tab (ordered by priority). The tool description shows the currently active provider.

Use `duckduckgo_search` when:
- You need a quick factual answer, definition, or summary
- No provider is active for `search_web`

Use `tavily_search` when:
- You need full extracted page content for deep analysis
- Research tasks requiring clean text, not just links

Typical flow:

1. search_web(query="query")                        — active provider
2. If insufficient results — retry with a different query or tool
3. For quick facts — duckduckgo_search(q="query")
4. For deep content extraction — tavily_search(query="query")
