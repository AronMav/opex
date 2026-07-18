---
name: web-search
description: Web search strategy — single search_web entry point backed by configurable providers (SearXNG / Ollama Cloud / Brave), with optional provider override via the `provider` parameter
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
priority: 10
state: active
last_used_at: "2026-07-18T08:04:57.156541646+00:00"
---

## Search Provider Selection

`search_web` is the single web-search entry point. It is routed to the highest-priority active provider (SearXNG, Ollama Cloud, Brave, …), configured in the Providers tab.

| Need | Tool | Why |
|------|------|-----|
| General web search | `search_web` | Primary entry point — routed to the highest-priority active provider |
| Specific provider | `search_web(provider="<name>")` | Pass the provider name to override the default; active providers are listed in the tool description |

## Strategy

Use `search_web` as the single web-search entry point. The active provider is configured in the Providers tab (ordered by priority). The tool description lists available providers and the default.

To select a non-default provider, pass `provider="<name>"` explicitly:
- `search_web(query="...", provider="ws-searxng")` — force SearXNG
- `search_web(query="...", provider="ws-ollama")` — force Ollama Cloud
- `search_web(query="...", provider="ws-brave")` — force Brave

Typical flow:
1. search_web(query="query")                        — default provider
2. If insufficient results — retry with a different provider via the `provider` param
