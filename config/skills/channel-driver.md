---
name: channel-driver
description: Create new channel adapter drivers (TypeScript/Bun) for Telegram, Discord, etc.
triggers:
  - создай драйвер канала
  - новый канал
  - channel driver
tools_required:
  - code_exec
priority: 10
---

# Creating Channel Drivers

Load the full guide first: `skill_use(action="load", name="channels-guide")`

## Workflow

1. Create driver file: `~/opex/channels/src/drivers/{name}.ts` via code_exec
2. **Add import and case** to `~/opex/channels/src/index.ts`
3. **Verify case was added**: grep for the new name in index.ts
4. **Do NOT modify formatting.ts** — new channels use standard CommonMark
5. Restart channels: `POST /api/services/channels/restart`
6. Verify health: `curl http://localhost:3100/health` — must return `{"ok":true}`

## Rules

- **NEVER overwrite existing channel files entirely** (formatting.ts, common.ts, bridge.ts, index.ts) — only targeted additions (add line to switch-case, import)
- These files contain escaped backticks in template literals — full overwrite breaks escaping
- Bun runs TypeScript directly, no compilation needed
