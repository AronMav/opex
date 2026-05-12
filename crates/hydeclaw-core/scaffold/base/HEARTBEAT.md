# Heartbeat: Maintenance Tasks

## Protocol

**Step 1. Backup health check**

```
GET /api/backup
GET /api/config
```

1. If `backup.enabled == true` — check that at least one backup exists and its `created_at` is within expected interval (based on cron schedule + 2 hour tolerance).
2. If no backups exist OR newest backup is older than `cron_interval + 2 hours` — notify user: "Backup is overdue. Last backup: {filename} ({age}). Expected: every {cron_description}."
3. If backup files exist but newest is older than 48h despite being enabled — escalate: "Backup system appears broken. No recent backups created."
4. Do NOT create backups — that is handled by the automated scheduler. Only monitor and report issues.

**Step 2. Long-term memory check**

```
memory(action="search", query="*", limit=50)
```

Look for semantic duplicates: entries with high content similarity (>0.92) and close dates. If duplicates are found:

1. Read both entries
2. Merge into one (the more complete version)
3. Delete the old one: `memory(action="delete", id=...)`
4. Save the merged one: `memory(action="index", ...)`

Do not merge entries from different agents. Do not touch entries with `pinned: true`.

**Step 3. Report**

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