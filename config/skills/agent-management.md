---
name: agent-management
description: Create, update, delete agents via Core API with GET→modify→PUT pattern
triggers:
  - создай агента
  - новый агент
  - настрой агента
  - измени агента
  - agent
tools_required:
  - code_exec
priority: 10
---

# Agent Management

Use Python `requests` in `code_exec`. Do NOT use curl.

## Auth helper

```python
import requests, json, os
TOKEN = os.environ.get("OPEX_AUTH_TOKEN", "")
H = {"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"}
BASE = "http://localhost:18789"
```

## Create agent

**CRITICAL: Use `provider_connection` (named provider from /api/providers), NOT `provider` (legacy field).**

The `provider` field is auto-filled from the named connection — you only need to set `provider_connection` and `model`.

```python
resp = requests.post(f"{BASE}/api/agents", headers=H, json={
    "name": "NewAgent",
    "language": "ru",
    "provider": "",
    "model": "kimi-k2.5:cloud",
    "provider_connection": "ollama-default",
    "temperature": 1.0
})
print(resp.status_code, resp.text)
```

**Fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `name` | yes | Agent name (alphanumeric + underscore/hyphen) |
| `language` | no | Default: "ru" |
| `provider` | yes* | Leave empty string "" — auto-filled from provider_connection |
| `model` | yes | Model name (e.g. "gpt-4.1", "llama3.3") |
| `provider_connection` | recommended | Name of provider from /api/providers (e.g. "ollama-default") |
| `temperature` | no | Default: 1.0 |

*`provider` is required by the API schema but auto-filled from `provider_connection` when empty.

## Find your provider connection

```python
providers = requests.get(f"{BASE}/api/providers", headers=H).json()["providers"]
for p in providers:
    if p["type"] == "text":
        print(f"name={p['name']}  type={p['provider_type']}  model={p['default_model']}")
```

Use the `name` field as `provider_connection` value.

## Update agent (GET→modify→PUT)

```python
# Get current config
agent = requests.get(f"{BASE}/api/agents/NewAgent", headers=H).json()

# Modify fields
agent["model"] = "new-model"
agent["provider_connection"] = "my-openai"

# Save
resp = requests.put(f"{BASE}/api/agents/NewAgent", headers=H, json=agent)
print(resp.status_code)
```

## Delete agent

```python
requests.delete(f"{BASE}/api/agents/NewAgent", headers=H)
```

## Common Errors

| Error | Cause | Fix |
|-------|-------|-----|
| Provider shows "—" in UI | `provider` field has wrong value | Set `provider_connection` and leave `provider` empty |
| Agent can't call LLM | No provider_connection set | Add `provider_connection` pointing to a provider from /api/providers |
| SyntaxError (curl) | Using curl in code_exec | Use Python `requests` |
