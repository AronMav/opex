---
name: access-block-handler
description: Handle web search access blocks — rate limits, 403 errors, auth failures. Provides fallback strategy when primary provider is unavailable.
triggers:
  - access blocked
  - rate limit
  - 403 error
  - too many requests
  - provider unavailable
  - доступ заблокирован
  - превышен лимит
  - ошибка доступа
tools_required:
  - search_web
priority: 5
state: active
---

## Access Block Recovery Strategy

When a web search provider returns an access block error (403, 429, rate limit, auth failure):

### Step 1: Identify the block
- Error contains "403", "429", "rate limit", "blocked", "unauthorized"
- Provider is configured but returning errors

### Step 2: Apply fallback chain
1. **Retry `search_web` once** — provider failover is server-side and automatic (toolgate walks the active-provider priority list; `search_web` has NO provider parameter)
2. **Wait and retry** — provider errors are often transient; search_web walks the active-provider priority list server-side, so a retry may hit a healthy provider
3. **Report to user** — if all providers fail, inform user: "Web search temporarily unavailable. Provider errors: {details}"

### Step 3: Log the incident
- Note which provider failed and the error
- Do NOT retry the same provider immediately (cooldown: 5 minutes)

### Provider Priority
| Priority | Provider | Notes |
|----------|----------|-------|
| 1 | ws-searxng | Self-hosted, no rate limits |
| 2 | ws-ollama | Cloud fallback |
| 3 | ws-brave | API key required |

### Common Errors
- `403 Forbidden` — API key invalid or expired
- `429 Too Many Requests` — rate limit exceeded, wait or switch provider
- `Connection refused` — provider service down
- `Timeout` — provider slow, try alternative
