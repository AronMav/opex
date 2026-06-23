---
name: cron-management
description: Create and manage scheduled tasks (cron jobs) for agents
triggers:
  - создай задачу по расписанию
  - cron
  - расписание
  - scheduled task
  - таймер
tools_required:
  - code_exec
priority: 10
---

# Cron Task Management

Use the `cron` tool with action parameter.

## Create task

```
cron(action="add", agent="AgentName", name="daily-report", cron="0 9 * * *", timezone="Europe/Samara", message="Generate daily report and send via Telegram")
```

## Critical rules

### Agent assignment

Tasks execute in the context of the **assigned agent**. If a task must send Telegram messages — assign it to the agent that HAS a Telegram channel, NOT to the base agent.

### Cron context has NO channel

Cron sessions run with `channel: "cron"`. There is NO Telegram/Discord context. To send a proactive message, the prompt must explicitly use `session(action="send")`:

```
session(action="send", message="...", user_id="123456789", channel="telegram")
```

### owner_id is REQUIRED

Before creating a cron task that sends notifications, verify the target agent has `owner_id`:

```bash
curl -sf http://localhost:18789/api/agents/AgentName \
  -H "Authorization: Bearer $OPEX_AUTH_TOKEN" | \
  python3 -c "import sys,json; print(json.load(sys.stdin).get('access',{}).get('owner_id'))"
```

If `None` — ask the user for their Telegram chat_id first.

### Prompt must be explicit

BAD: "Check portfolio and tell the user."
GOOD: "Check BCS portfolio (bcs_portfolio tool). If total_rub > 555000, use session(action='send', user_id='123456789', channel='telegram') to notify."

## Other actions

```
cron(action="list")                    # list all jobs
cron(action="history", job_id="UUID")  # recent runs
cron(action="run", job_id="UUID")      # run now (manual trigger)
cron(action="update", job_id="UUID", enabled=false)  # disable
cron(action="remove", job_id="UUID")   # delete
```

## Checklist

1. Task assigned to the correct agent (one with the target channel)
2. Target agent has `owner_id` set
3. Prompt explicitly mentions `session(action="send")` with `user_id` and `channel`
4. Run once manually (`cron action=run`) and confirm message arrives
