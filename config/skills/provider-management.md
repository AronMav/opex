---
name: provider-management
description: Create, update, delete LLM and media providers via unified /api/providers API
triggers:
  - создай провайдера
  - добавь провайдер
  - подключи модель
  - настрой провайдер
  - provider
tools_required:
  - code_exec
priority: 10
---

# Provider Management

All providers (LLM and media) use the unified `/api/providers` endpoint. There are NO separate `/api/llm-providers` or `/api/media-providers` endpoints.

## Provider types

**LLM (type="text"):** openai, anthropic, google, minimax, deepseek, groq, together, openrouter, mistral, xai, perplexity, ollama

**Media:** stt, tts, vision, imagegen, embedding — each with its own driver set (see `GET /api/provider-types`)

## Create provider

```bash
curl -sf -X POST http://localhost:18789/api/providers \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "my-openai",
    "type": "text",
    "provider_type": "openai",
    "base_url": "https://api.openai.com/v1",
    "api_key": "sk-...",
    "default_model": "gpt-4o",
    "enabled": true
  }'
```

For media providers, set `type` to the capability (stt, tts, vision, imagegen, embedding) and `provider_type` to the driver name.

```bash
# Example: fal.ai image generation
curl -sf -X POST http://localhost:18789/api/providers \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "fal-flux",
    "type": "imagegen",
    "provider_type": "fal",
    "api_key": "key-here",
    "default_model": "fal-ai/flux/schnell",
    "enabled": true
  }'
```

## Activate media provider

Media provider selection is per-**Profile** now (since m084), NOT a global
active flag. A Profile's `slots` map each capability to an ordered list of
providers, e.g. `{"imagegen": [{"provider": "fal-flux"}]}`. Add the new
provider to the relevant capability slot of the profile(s) that should use it.
`PUT /api/profiles/{id}` REPLACES the whole `slots` object, so fetch the
current slots first, add/modify the capability, then send the full object back:

```bash
# 1. List profiles → find the id and current slots
curl -sf http://localhost:18789/api/profiles \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"

# 2. Send the FULL slots back with the new provider added for the capability
curl -sf -X PUT http://localhost:18789/api/profiles/PROFILE_ID \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"slots": {"imagegen": [{"provider": "fal-flux"}]}}'
```

> `PUT /api/provider-active` now accepts **only** `capability: embedding`
> (everything else returns 400 — media capabilities are managed through
> Profiles). Use it solely for the embedding provider.

Then reload toolgate so it picks up the new config:

```bash
curl -sf -X POST http://localhost:18789/api/services/toolgate/restart \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"
```

Poll health until ready (up to 15 seconds):

```bash
for i in $(seq 1 5); do
  sleep 3
  if curl -sf http://localhost:9011/health >/dev/null 2>&1; then
    echo "Toolgate healthy"
    curl -sf http://localhost:9011/health
    break
  fi
  echo "Waiting for toolgate... ($i/5)"
done
```

The `active_providers` field should show the new provider.

## Update provider

```bash
curl -sf -X PUT http://localhost:18789/api/providers/PROVIDER_UUID \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"api_key": "new-key", "default_model": "gpt-4o-mini"}'
```

UUID is required (not name). Get it from `GET /api/providers`.

## Update CLI provider options

For CLI providers (gemini-cli, claude-cli, codex-cli), use PATCH to update CLI-specific options.
Allowed fields: `command`, `args`, `prompt_arg`, `model_arg`, `env_key`.

A `command` override is validated (must exist on system). After update, a health-check runs automatically.

```bash
curl -sf -X PATCH http://localhost:18789/api/providers/PROVIDER_UUID \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "options": {
      "args": ["--output-format", "json", "--no-color"],
      "prompt_arg": "--prompt"
    }
  }'
```

Response includes `health_check` field with CLI validation result:

```json
{
  "provider": { ... },
  "health_check": {
    "cli_found": true,
    "cli_path": "/usr/bin/gemini",
    "auth_ok": true,
    "response_ok": true
  }
}
```

## Delete provider

```bash
curl -sf -X DELETE http://localhost:18789/api/providers/PROVIDER_UUID \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"
```

## Discover models

```bash
curl -sf http://localhost:18789/api/providers/PROVIDER_UUID/models \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"
```

Returns `{"models": [...]}` — array of strings or `{id, owned_by}` objects.

## Assign LLM provider to agent

After creating a text provider, assign it to an agent using the GET→modify→PUT pattern from the agent-management skill. Set `provider_connection` to the provider **name** (not UUID).

## List available driver types

```bash
curl -sf http://localhost:18789/api/provider-types \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"
```

## Checklist

1. Create provider via `POST /api/providers`
2. For media: add the provider to the target Profile's capability slot
   (`PUT /api/profiles/{id}`) + restart toolgate. `PUT /api/provider-active`
   is embedding-only now.
3. For LLM: assign to agent via `provider_connection` field
4. Verify: `GET /api/providers` shows the new record
5. For CLI providers: update options via `PUT /api/providers/{id}`
