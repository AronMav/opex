# Heartbeat: Maintenance Tasks

## Protocol

**Step 1. Infra jobs health (backup + curator) — ensure enabled & running**

Use `code_exec` (Python `requests`). Base `http://localhost:18789`; header
`Authorization: Bearer <OPEX_AUTH_TOKEN from env>`. Wrap each call in
try/except — one failure must not abort the others.

1. **Curator** — `GET /api/curator/status`:
   - If `enabled` is `false` → `PUT /api/curator/config` with the current values
     plus `enabled: true` (re-enable; this persists to config and reschedules).
   - If `last_run_at` is null or older than **8 days** (weekly cron + 1 day
     grace) → `POST /api/curator/run`.
2. **Backup** — `GET /api/config`:
   - If `backup.enabled` is `false` → `PUT /api/config` with `backup_enabled: true`.
   - `GET /api/backup`: if no backups exist, or the newest is older than
     `cron_interval + 2 hours` → `POST /api/backup` (create one now).
3. Report what you enabled or created. If both jobs were already enabled and
   current, nothing to do — continue to the next step.

Policy: unlike before, you now **ensure** these jobs are on and create a
backup if one is overdue — do not merely observe. The Watchdog also alerts if a
job is found disabled, but you are the one that fixes it.

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