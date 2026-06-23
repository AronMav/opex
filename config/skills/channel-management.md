---
name: channel-management
description: Connect Telegram, Discord, Matrix, IRC, Slack, WhatsApp channels to agents
triggers:
  - подключи телеграм
  - подключи канал
  - telegram bot
  - discord bot
  - channel
tools_required:
  - code_exec
priority: 10
---

# Channel Management

## Supported types

`telegram`, `discord`, `matrix`, `irc`, `slack`, `whatsapp`

Tokens are stored encrypted in the vault automatically.

## Create channel

```bash
# Telegram
curl -sf -X POST http://localhost:18789/api/agents/AgentName/channels \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "channel_type": "telegram",
    "display_name": "Telegram Bot",
    "config": {"bot_token": "123456:ABC-DEF"}
  }'

# Discord
curl -sf -X POST http://localhost:18789/api/agents/AgentName/channels \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "channel_type": "discord",
    "display_name": "Discord Bot",
    "config": {"bot_token": "MTIz..."}
  }'

# Matrix
curl -sf -X POST http://localhost:18789/api/agents/AgentName/channels \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "channel_type": "matrix",
    "display_name": "Matrix Bot",
    "config": {"access_token": "syt_...", "homeserver": "https://matrix.org"}
  }'
```

## List channels

```bash
curl -sf http://localhost:18789/api/agents/AgentName/channels \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"
```

## Update / Delete / Restart channel

```bash
# Update
curl -sf -X PUT http://localhost:18789/api/agents/AgentName/channels/CHANNEL_UUID \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"display_name": "New Name", "config": {"bot_token": "new-token"}}'

# Delete
curl -sf -X DELETE http://localhost:18789/api/agents/AgentName/channels/CHANNEL_UUID \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"

# Restart
curl -sf -X POST http://localhost:18789/api/agents/AgentName/channels/CHANNEL_UUID/restart \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN"
```

## Checklist

1. Get bot token from user (Telegram: @BotFather, Discord: Developer Portal)
2. Create channel via `POST /api/agents/{name}/channels`
3. Verify: channel status should become "running"
4. If agent needs proactive messaging — ensure `access.owner_id` is set on the agent
