# Session Mirroring (P1.2) — Design

> **Source:** Hermes Agent `gateway/mirror.py`, adapted for HydeClaw's Postgres architecture.
> **Goal:** When a cron job (or any outbound channel delivery) sends a message to a platform, append a mirror record to the recipient's session so the agent has cross-platform context on the next inbound turn.

---

## Context

HydeClaw cron jobs run `engine.run_for_cron()` in isolation and then deliver the result via `engine.send_channel_message(channel, chat_id, text)`. The result is stored in a temporary cron session — but the _target_ user's DM session never sees it. If the user later messages the agent on Telegram, the agent has no memory of what was already sent via cron.

In Hermes, `mirror.py` writes a `mirror=True` record directly to the target session's JSONL/SQLite transcript with `role: "assistant"`. The agent sees it as its own prior output on the next turn.

---

## Architecture

### 1. DB Migration

```sql
ALTER TABLE messages
  ADD COLUMN is_mirror BOOLEAN NOT NULL DEFAULT false;
```

No index needed — not queried in isolation, just read as part of normal `load_messages`.

### 2. `mirror_to_session` helper

New async function in `crates/hydeclaw-db/src/sessions.rs`:

```rust
pub async fn mirror_to_session(
    db: &PgPool,
    agent_id: &str,
    channel: &str,
    participant_id: &str,   // e.g. "telegram:123456789"
    text: &str,
) -> Result<bool>
```

**Logic:**
1. Find the most recent session for this agent + channel + participant:
   ```sql
   SELECT id FROM sessions
   WHERE agent_id = $1
     AND channel  = $2
     AND $3       = ANY(participants)
   ORDER BY started_at DESC
   LIMIT 1
   ```
2. If not found → return `Ok(false)` (silent miss, non-fatal).
3. If found → `INSERT INTO messages (session_id, role, content, is_mirror) VALUES ($1, 'assistant', $2, true)`.
4. Return `Ok(true)`.

### 3. Hook in `scheduler/mod.rs`

After `engine.send_channel_message(ch, cid, &text)` succeeds (line ~962), fire-and-forget:

```rust
let db   = db.clone();
let aid  = engine.cfg().agent.name.clone();
let ch   = ch.to_string();
let cid  = cid.to_string();
let txt  = text.to_string();
tokio::spawn(async move {
    let participant_id = format!("{ch}:{cid}");
    if let Err(e) = crate::db::sessions::mirror_to_session(
        &db, &aid, &ch, &participant_id, &txt,
    ).await {
        tracing::debug!(error = %e, "mirror_to_session failed (non-fatal)");
    }
});
```

`participant_id` format matches what channel adapters store in `sessions.participants`
(e.g. `"telegram:123456789"`). The exact format is validated at implementation time
against live session rows.

### 4. `load_messages` — no change

Mirror records load as normal `role='assistant'` rows. The agent sees them as its own
prior output and can reference them on the next inbound turn.

### 5. `MessageRow` — add `is_mirror` field

In `crates/hydeclaw-db/src/sessions.rs`:
```rust
pub struct MessageRow {
    // ... existing fields ...
    #[sqlx(default)]
    pub is_mirror: bool,
}
```

`#[sqlx(default)]` ensures backward compatibility with any query that doesn't SELECT the column.

---

## Frontend

### Types

`api.generated.ts` — add to `MessageRow`:
```typescript
is_mirror?: boolean;
```

`chat-types.ts` — propagate to `ChatMessage` or `MessagePart` where messages are rendered.

### Render

If `message.is_mirror === true`, show a small inline badge next to the message:
```tsx
{message.is_mirror && (
  <span className="text-[10px] text-orange-500 ml-1">↩ cron</span>
)}
```

Same style as `end_reason='compression'` marker in `CompactChainBanner`.

### Session list preview

Mirror messages are excluded from "last message" preview text in the session list — they represent outbound cron delivery, not conversation turns. Filter: `WHERE is_mirror = false` in the last-message subquery (or client-side).

---

## Error Handling

| Scenario | Behaviour |
|---|---|
| No matching session | `Ok(false)`, debug-log, silent |
| DB insert fails | logged in spawned task, never surfaces to caller |
| `send_channel_message` fails | mirror is never attempted (not reached) |
| Duplicate mirrors (bug) | harmless — agent sees repeated assistant text, which is benign |

Mirror is always after delivery — it never blocks or delays the channel send.

---

## Scope

**In scope:**
- Outbound cron delivery (`send_channel_message` hook)
- Heartbeat jobs (same delivery path)
- Watchdog alerts (same delivery path)

**Out of scope:**
- Inbound mirror (channel → session already happens natively)
- Cross-agent mirroring (agent A mirrors to agent B's session)
- Retroactive mirror for past deliveries

---

## Testing

**Rust unit test** (`crates/hydeclaw-db/src/sessions.rs` or integration test):
1. `mirror_to_session` finds session by participants → inserts row with `is_mirror=true`
2. `mirror_to_session` with no matching session → returns `Ok(false)`, no insert

**Vitest** (`ui/src/__tests__/session-mirroring.test.tsx`):
1. Message with `is_mirror=true` renders "↩ cron" badge
2. Message with `is_mirror=false` renders no badge

---

## File Map

| File | Action |
|---|---|
| `migrations/043_messages_is_mirror.sql` | CREATE — add `is_mirror` column |
| `crates/hydeclaw-db/src/sessions.rs` | MODIFY — `MessageRow.is_mirror` + `mirror_to_session()` |
| `crates/hydeclaw-core/src/scheduler/mod.rs` | MODIFY — tokio::spawn mirror after send_channel_message |
| `ui/src/types/api.generated.ts` | MODIFY — `is_mirror?: boolean` on MessageRow |
| `ui/src/stores/chat-types.ts` | MODIFY — propagate `is_mirror` |
| `ui/src/components/chat/` | MODIFY — render badge |
| `ui/src/__tests__/session-mirroring.test.tsx` | CREATE — Vitest coverage |

---

## History

- **2026-05-03** — Design created via `/superpowers:brainstorming`.
  Reference: `D:/GIT/hermes-agent/gateway/mirror.py`.
