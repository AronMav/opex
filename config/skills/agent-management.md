---
name: agent-management
description: Create, update, delete agents via Core API with GET→modify→PUT pattern. Model is chosen by named PROFILE (not provider/model fields).
triggers:
  - создай агента
  - новый агент
  - настрой агента
  - измени агента
  - смени модель агента
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

## Model resolution = PROFILE (not provider/model)

**An agent's LLM (and STT/TTS/vision/imagegen/websearch) is resolved from a named
PROFILE**, a row in the `profiles` table. The legacy per-agent `provider` /
`provider_connection` / `model` fields are gone from the create/update payload —
setting them does nothing. Set the `profile` field to a profile name instead.

List profiles and see what each binds:

```python
profiles = requests.get(f"{BASE}/api/profiles", headers=H).json()
for p in profiles.get("profiles", profiles):
    print(p["name"], "->", p.get("slots", p))
```

Each profile has capability *slots* (`text`, `compaction`, `stt`, `tts`, `vision`,
`imagegen`, `websearch`); the `text` slot's first entry is the agent's primary LLM,
and `text[1..]` are failover models. To change an agent's model, either point the
agent at a different profile (`profile` field) or edit the profile itself via
`PUT /api/profiles/{id}` (that changes it for every agent on that profile).

## Create agent

`profile` omitted/empty → the default profile. Give a real profile name to bind a
specific model set.

```python
resp = requests.post(f"{BASE}/api/agents", headers=H, json={
    "name": "NewAgent",
    "language": "ru",
    "profile": "Arty",        # a name from GET /api/profiles
    "temperature": 1.0,
})
print(resp.status_code, resp.text)
```

New agents are created **restricted** by default (see Access below) and non-base
agents get an auto deny-list (`code_exec`, `process_start`, `workspace_delete`,
`workspace_rename`).

**Top-level create fields:** `name` (required), `language`, `profile`,
`temperature`, `max_tokens`, `prompt_cache`, `daily_budget_tokens`,
`max_failover_attempts`, `max_tools_in_context`, `voice`, `block_tools`.
Everything else is a nested **section object** (below).

## Update agent — GET → modify → PUT (always)

PUT replaces the config, but omitted pieces are preserved from disk: the `base`
flag and `profile` are always kept, and the personality sections
(`soul`/`drift`/`initiative`/`emotion`) use presence-gated merge — a section you
DON'T send is kept as-is; a section you DO send wins. So always start from GET.

```python
agent = requests.get(f"{BASE}/api/agents/NewAgent", headers=H).json()
agent["profile"] = "Lana"                 # switch model set
agent["temperature"] = 0.9                # tune sampling
resp = requests.put(f"{BASE}/api/agents/NewAgent", headers=H, json=agent)
print(resp.status_code, resp.text)
```

## Config sections (nested objects on the agent)

Each is a nested object; GET the agent, modify the object, PUT it back.

| Section | Key fields | What it does |
|---------|-----------|--------------|
| `access` | `mode` ("open"/"restricted"), `owner_id` | Who may talk to the agent. `restricted` without an `owner_id` blocks channels until pairing. |
| `tools` | `deny` (list), `allow_all`, `deny_all_others`, `groups` | Tool policy. `deny` is checked first, before core allowlist. |
| `tool_dispatcher` | `enabled`, `core_extra`, `promotion_max` | `enabled=false` (default) → all tools are NATIVE. `enabled=true` → most tools are reached via the `tool_use` dispatcher and only static-core + `core_extra` stay native. Weak-tool-adherence models usually do BETTER with dispatcher OFF. |
| `tool_loop` | `max_iterations`, `break_threshold`, `max_consecutive_failures`, … | Loop-detection + iteration caps. |
| `compaction` | `enabled`, `threshold`, `preserve_last_n`, … | Context compaction tuning. |
| `soul` | `enabled`, `reflection_threshold`, `context_top_k`, … | Autobiographical memory + reflection (SELF.md). Off by default. |
| `drift` | `enabled`, `correct`, `anchor`, `threshold` | Persona anti-drift. Needs `soul.enabled=true` to function; `drift` with soul off is inert. |
| `initiative` | `enabled`, `daily_plan`, `decompose`, `daily_proposal_cap` | Proactive daily-plan / open-threads. |
| `emotion` | `enabled`, `intensity_importance_k`, `blend_rate`, `decay_half_life_hours` | Emotional appraisal layer. |
| `heartbeat` | `cron`, `timezone`, `announce_to` | Scheduled autonomous wakeups. |
| `approval` | `enabled`, `require_for`, `timeout_seconds` | Human-in-the-loop tool approvals. |

Example — turn a persona's temperature down and enable its soul:

```python
a = requests.get(f"{BASE}/api/agents/Lana", headers=H).json()
a["temperature"] = 1.0
a["soul"] = {**(a.get("soul") or {}), "enabled": True}
requests.put(f"{BASE}/api/agents/Lana", headers=H, json=a)
```

## Delete agent

```python
requests.delete(f"{BASE}/api/agents/NewAgent", headers=H)
```

Base agents (`base=true`) cannot be renamed or deleted; their `SOUL.md` /
`IDENTITY.md` / `SELF.md` are immutable.

## Common Errors

| Symptom | Cause | Fix |
|---------|-------|-----|
| Agent uses the wrong / default model | Set `provider`/`model`/`provider_connection` (gone) instead of `profile` | Set `profile` to a name from `GET /api/profiles` |
| Model change didn't take | Edited the agent but the profile binds the model | Repoint `profile`, or edit the profile via `PUT /api/profiles/{id}` |
| Agent silent on a channel | `access.mode="restricted"` and not paired | Pair via the channel, or set `access.owner_id` |
| Personality section reset after PUT | Sent a partial config not built from GET | Always GET → modify → PUT |
| SyntaxError (curl) | Using curl in code_exec | Use Python `requests` |

## Note

Agent TOML changes made by editing files on disk are NOT hot-reloaded (the config
watcher only watches `opex.toml`, not `config/agents/*.toml`). Use this API
(`PUT /api/agents/{name}`) so changes apply immediately, or restart core.
