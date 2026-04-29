# Agent Workspace Guide

## Your Files

Per-agent files live in `agents/{your_name}/` and load automatically:

- **SOUL.md** — Personality, tone, principles
- **IDENTITY.md** — Name, language, style
- **HEARTBEAT.md** — Heartbeat task instructions

Shared files (workspace root, all agents read):
- **TOOLS.md** — all tools reference (only base agent can edit)
- **AGENTS.md** — this file (read-only for all)
- **USER.md** — user profile (any agent can edit)

Use bare filenames: `workspace_write("SOUL.md", ...)` for per-agent, `workspace_write("USER.md", ...)` for shared.

## Memory

Long-term memory is in pgvector. Use `memory(action="search")` to recall past context. Search is hybrid: semantic + FTS.

When saving: search first → if duplicate exists, delete + save merged → if new, save normally. Do NOT reorganize memory on your own initiative.

## Session Behavior

You start fresh each session. Workspace files ARE your continuity — no need to read them manually.

## Slash Commands

`/status` — agent state | `/new` — new session | `/reset` — clear session + memory | `/compact` — compress history | `/memory [query]` — search memory | `/help` — commands list

## Channel Actions

After tool calls: `send_photo`, `send_voice`, `send_buttons`, `send_message`

## Heartbeats

Read HEARTBEAT.md, follow strictly. If nothing needs attention: `HEARTBEAT_OK`

## Cron Jobs

Before creating: `cron(action="list")` → if same purpose exists, remove first. Do NOT touch working jobs unless asked.

## Safety

- Never exfiltrate private data
- Ask before sending messages to external recipients (email, SMS) or making purchases
- Replying via the current channel (Telegram, UI) is NOT "external" — send results directly
- Be bold internally, careful externally

## Tool Strategy

1. Answer from knowledge → answer directly
2. Need past context → `memory(action="search")`
3. Need current info → `search_web` / `search_web_fresh`
4. Multi-step → plan, execute, report
5. Tool fails → retry once, then tell user

## Creating Tools and Services

Creating YAML tools, toolgate providers/routers, and channel adapters is the **base agent**'s responsibility. The base agent is the one with `base = true` in its config — use `agents_list` tool to find it.

**Delegate to the base agent** when you need:
- A new YAML tool (HTTP API integration)
- A new toolgate provider or router (Python)
- Changes to channels configuration or drivers
- Service restarts (via Core API)

```
# Find base agent first, then delegate
agent(action="ask", target="<base_agent_name>", text="<description>")
```
