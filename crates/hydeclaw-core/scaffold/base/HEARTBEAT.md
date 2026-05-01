# Heartbeat: Maintenance Tasks

## Initialization (on first launch)

Check for the existence of a daily backup cron job:
`cron(action="list")` → if there is no job named `"daily-backup"`:

1. Read `USER.md` → find the line `Timezone: <value>`
2. If timezone is **not specified or empty** — notify the user:
   > "Failed to create backup job: Timezone is not specified in USER.md. Please specify a timezone (e.g.: UTC) — I will create the job automatically."
   Do not create the job.
3. If timezone is specified — create the job:

```
cron(action="create", name="daily-backup", expr="0 5 * * *",
     timezone="<timezone from USER.md>", message="BACKUP", announce_to="{AGENT_NAME}", silent=false)
```

## Daily Backup (upon receiving the BACKUP message)

Call via code_exec: `curl -sf -X POST http://localhost:18789/api/backup -H "Authorization: Bearer $HYDECLAW_AUTH_TOKEN"`

- If `ok == true` — record `filename` and `size_bytes`. Respond briefly.
- If error — notify the user via `message`.

Backups are retained for 7 days. Files older than that are deleted automatically.

---

Upon receiving a heartbeat — perform maintenance tasks.

## Protocol

**Step 1. Long-term memory check**

```
memory(action="search", query="*", limit=50)
```

Look for semantic duplicates: entries with high content similarity (>0.92) and close dates. If duplicates are found:

1. Read both entries
2. Merge into one (the more complete version)
3. Delete the old one: `memory(action="delete", id=...)`
4. Save the merged one: `memory(action="index", ...)`

Do not merge entries from different agents. Do not touch entries with `pinned: true`.

**Step 2. Report**

If everything is normal: respond with `HEARTBEAT_OK`.

If anomalies were detected and resolved: briefly describe — what happened, what was done.

## Note

System health monitoring (services, resources, containers, restarts, secrets, alerting) is handled by **Watchdog** — a built-in Core subsystem. {AGENT_NAME} does NOT perform health checks or service restarts during heartbeat.

## Skill Maintenance

Check `GET /api/skills/repairs?status=pending`. If the queue is non-empty,
process all pending repairs using the skill-curator skill before other tasks.
Report: how many repairs processed, how many succeeded, how many failed.

## Diagnostic Principles

- Do not interpret — measure.
- Do not guess the cause — record the symptom.
- Record the result of each check in this file only when anomalies are detected.
