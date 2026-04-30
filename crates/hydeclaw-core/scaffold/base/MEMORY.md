# {AGENT_NAME} — Reference

## System Architecture

```text
Core (Rust, :18789)
├── channels (Bun, native process) — ~/hydeclaw/channels/
├── toolgate (Python, :9011, native process) — ~/hydeclaw/toolgate/
├── PostgreSQL (Docker) + pgvector (memory)
└── Docker sandbox — for regular agents, NOT for {AGENT_NAME}
```

**Key paths on Pi:**
- Binary: `~/hydeclaw/hydeclaw-core-aarch64`
- UI static: `~/hydeclaw/ui/out/`
- Config: `~/hydeclaw/config/`
- Workspace: `~/hydeclaw/workspace/`
- Migrations: `~/hydeclaw/migrations/`
- Docker: `~/hydeclaw/docker/`

## Core API Reference

Base: `http://localhost:18789` — Auth: Bearer `$HYDECLAW_AUTH_TOKEN`

| Resource | Endpoints |
|----------|-----------|
| Providers | `GET/POST /api/providers`, `GET/PUT/DELETE /api/providers/{uuid}`, `GET /api/providers/{uuid}/models`, `GET /api/provider-types`, `GET/PUT /api/provider-active` |
| Agents | `GET/POST /api/agents`, `GET/PUT/DELETE /api/agents/{name}` |
| Channels | `GET/POST /api/agents/{name}/channels`, `PUT/DELETE /api/agents/{name}/channels/{uuid}`, `POST .../restart` |
| Other | `GET /api/doctor`, `GET /api/sessions?agent={name}`, `GET/POST /api/secrets`, `GET /api/tool-definitions`, `POST /api/services/{name}/restart` |

## Available Tools

**Files:**
- `code_exec` — bash/python on host
- `workspace_write` — create/overwrite workspace/ files
- `workspace_read / workspace_list` — read workspace files
- `workspace_edit` — precise line editing

**YAML tool management:**
- `tool_list` — show all YAML tools
- `tool_test` — test a YAML tool

**Communication:**
- `agent` — talk to peer agents in this session (ask/status/kill)
- `message` — reply to user
- `web_fetch` — HTTP requests

**Consolidated tools (use `action` parameter):**
- `memory(action=search/index/reindex/get/delete/update)`
- `session(action=list/history/search/context/send/export)`
- `cron(action=list/history/add/update/remove/run)`

**Other:**
- `secret_set`, `canvas`, `rich_card`, `browser_action`

## Denied Tools

`workspace_delete`, `workspace_rename`, `git`, `tool_create`, `tool_verify`, `tool_disable`,
`tool_discover` (without explicit request), `skill`, `process` — use `code_exec` or
`workspace_write/edit` alternatives.

## Methodology

### Goal-Backward Reasoning
Define the end state first: "What must be TRUE when this is done?" Work backward to required steps.

### Discovery Classification
- **Level 0** (known path): Execute directly.
- **Level 1** (known domain): Brief exploration (2-3 files), then execute.
- **Level 2** (unknown approach): Research first — read docs, examine patterns, then plan.
- **Level 3** (unknown domain): Ask clarifying questions before any action.

### Verification Mindset
Every step needs "how to prove it works." Verify with concrete evidence (command output, observable
behavior). Details: `skill_use("verification")`.

### Error Recovery
Diagnose from error message; fix in next attempt — never repeat verbatim. After 2 failed attempts,
try a fundamentally different strategy or report the blocker.

### Multi-Agent Awareness
Delegate tasks outside your expertise via `agent(action="ask")`. Details:
`skill_use("multi-agent-coordination")`.
