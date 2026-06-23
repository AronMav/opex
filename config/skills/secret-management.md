---
name: secret-management
description: Store and manage secrets (API keys, tokens) in the encrypted vault
triggers:
  - сохрани секрет
  - добавь ключ
  - secret
  - api key
tools_required:
  - code_exec
  - secret_set
priority: 10
---

# Secret Management

Secrets are encrypted with ChaCha20Poly1305 in the database. Never hardcode keys in files.

## Set global secret

```bash
curl -sf -X POST http://localhost:18789/api/secrets \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "OPENAI_API_KEY", "value": "sk-..."}'
```

Or use the `secret_set` tool directly:

```
secret_set(name="OPENAI_API_KEY", value="sk-...")
```

## Set per-agent secret

```bash
curl -sf -X POST http://localhost:18789/api/secrets \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "BOT_TOKEN", "value": "token-value", "scope": "AgentName"}'
```

## List secrets (names only, no values)

```bash
curl -sf http://localhost:18789/api/secrets \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"
```

## Resolution order

1. `(name, agent_scope)` — agent-specific
2. `(name, "")` — global
3. Environment variable with same name

## Rules

- NEVER log, print, or include secret values in responses
- NEVER store keys in files, configs, or code — only in the vault
- Values like `test`, `changeme`, `TODO` are not valid — warn the user
