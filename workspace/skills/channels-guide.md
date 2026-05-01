---
name: channels-guide
description: Guide for creating and modifying channel drivers (Telegram, Discord, Matrix, IRC, Slack, WhatsApp)
triggers:
  - channel driver
  - new channel
  - create channel
  - telegram driver
  - discord driver
  - channel adapter
tools_required:
  - code_exec
  - web_fetch
priority: 5
state: active
---

Channels is a TypeScript/Bun process, the channel adapter for HydeClaw. Bun runs TypeScript directly — no compilation needed.

## Adding a New Driver

1. Create `~/hydeclaw/channels/src/drivers/{type}.ts` via code_exec
2. Add to `~/hydeclaw/channels/src/index.ts`:
   - import: `import { createXxxDriver } from "./drivers/xxx";`
   - case in `getDriverFactory()`: `case "xxx": return createXxxDriver;`
3. Restart channels via Core API

## Driver Interface

```typescript
export function createXxxDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  typingMode: string,
): {
  start: () => Promise<void>;
  stop: () => Promise<void>;
  onAction?: (action: OutboundAction) => Promise<void>;
}
```

## BridgeHandle — Key Methods

- `bridge.sendMessage({user_id, display_name, text, attachments, context, timestamp})` — send to agent, get streaming response
- `bridge.checkAccess(userId)` — check user access
- `bridge.cancelRequest(requestId)` — cancel
- `bridge.sendActionResult(actionId, success, error?)` — channel action result

## Utilities (common.ts)

- `splitText(text, maxLen, preserveCode)` — split long messages
- `commonMarkToMarkdownV2(text)` — Telegram formatting
- `commonMarkToDiscord(text)` — Discord formatting
- `toolEmoji(toolName)` — emoji for tool calls
- `parseUserCommand(text)` — parse /stop, /think, /help

## Registering a Channel in DB

POST `/api/agents/{name}/channels` with `channel_type`, `display_name`, `config: {bot_token: "..."}`.
