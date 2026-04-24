# Per-agent session entry cap

## Goal

Prevent the sessions table from growing without bound when an agent is
driven by cron jobs or async subagents. Current age-based prune
(`ttl_days = 30`, daily cron at 05:00 UTC) does not protect against
high-velocity session creation between runs.

## Background

We already have two cleanup passes:

- **Daily session prune** (`cleanup_old_sessions`, 05:00 UTC): deletes
  sessions older than `agent.session.ttl_days`.
- **Hourly WAL prune** (`prune_old_events_batched`, top of the hour):
  deletes `session_events` rows older than
  `cleanup.session_events_retention_days`.

Neither enforces a maximum count. An agent that spawns sessions faster
than they age out (e.g., a 5-minute cron calling `agent(action="async")`)
can accumulate thousands of live sessions in the 24h window between age
prunes. On a Raspberry Pi this is a real OOM / WAL-bloat vector.

## Change

### Config

Add to `LimitsConfig` (`crates/hydeclaw-core/src/config/mod.rs`):

```rust
/// Maximum sessions retained per agent. Oldest excess sessions are
/// deleted at startup and during the daily session prune cron.
/// 0 disables the cap. Default: 500.
#[serde(default = "default_max_sessions_per_agent")]
pub max_sessions_per_agent: u32,
```

Default = `500`. Covers the realistic range:

- Human-driven agents observed in production: Arty 83, Hyde 94.
- Cron-heavy worst case: 24 turns/day × 30 days = 720 — the age prune
  would have trimmed the oldest before that count is reached, but 500
  keeps a safety margin.

### DB function

New in `crates/hydeclaw-db/src/sessions.rs`:

```rust
/// Delete sessions beyond `max_per_agent` for every agent, keeping the
/// most recent by `last_message_at`. Never touches running sessions.
/// Returns total rows deleted. A cap of 0 is a no-op.
pub async fn cleanup_excess_sessions_per_agent(
    db: &PgPool,
    max_per_agent: u32,
) -> Result<u64> {
    if max_per_agent == 0 { return Ok(0); }
    let result = sqlx::query(
        "WITH ranked AS ( \
           SELECT id, ROW_NUMBER() OVER ( \
             PARTITION BY agent_id ORDER BY last_message_at DESC \
           ) AS rn \
           FROM sessions \
           WHERE run_status != 'running' OR run_status IS NULL \
         ) \
         DELETE FROM sessions \
         WHERE id IN (SELECT id FROM ranked WHERE rn > $1)",
    )
    .bind(max_per_agent as i32)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
```

Running sessions are preserved — they may be actively streaming or
holding LLM state. They are not counted toward the cap either (safe:
at most a handful of sessions are `running` at any moment).

### Call sites

**1. Startup (`main.rs`, after DB migrations, before gateway bind):**
one-shot enforcement so a heavily-accumulated backlog doesn't keep
bleeding memory after a restart.

**2. Daily cron (`scheduler/mod.rs::add_session_cleanup`):** right after
`cleanup_old_sessions`. Same 05:00 UTC window, cascade delete of
messages already handled by FK ON DELETE CASCADE.

## Compatibility

- FK cascades: `messages`, `session_events`, message branches — all
  existing FKs with `ON DELETE CASCADE` fire.
- Existing installations: default 500 is larger than any currently-
  observed agent on the Pi, so the first run will delete nothing unless
  an operator is already over the cap.
- `max_sessions_per_agent = 0` disables the feature for operators who
  relied on unbounded retention.

## Verification

After deploy:

```sql
SELECT agent_id, COUNT(*) FROM sessions GROUP BY agent_id ORDER BY 2 DESC;
```

Expected: no agent exceeds `limits.max_sessions_per_agent`. Running
sessions may bring the total over if count-at-the-moment is tight,
but that's acceptable — they're preserved intentionally.

Log line expected on startup when cap fires:

```
INFO hydeclaw_core::main: session cap enforcement deleted=N cap=500
```

## Non-goals

- Per-agent cap overrides (one global cap is enough; operators who
  need special-case behavior can set `ttl_days` per agent config).
- Global (cross-agent) cap — multi-agent isolation is simpler.
- Hot-path enforcement on session create — adds DB round-trip per
  session; startup + daily cron cover the realistic risk window.

## Scope

- ~15 lines config
- ~25 lines DB function
- ~10 lines main.rs startup call
- ~5 lines scheduler cron addition
- ~50 lines total, one commit.
