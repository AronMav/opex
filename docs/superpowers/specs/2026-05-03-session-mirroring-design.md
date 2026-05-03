# Session Mirroring (P1.2) — Design

> **Source:** Hermes Agent `gateway/mirror.py`, adapted for HydeClaw's Postgres architecture.
> **Goal:** When a cron job (or any outbound channel delivery) sends a message to a platform,
> append a mirror record to the recipient's session so the agent has cross-platform context
> on the next inbound turn.

---

## Context

HydeClaw cron jobs run `engine.run_for_cron()` in isolation and deliver the result via
`engine.send_channel_message(channel, chat_id, text)`. The result is stored in a temporary
cron session — but the _target_ user's DM session never sees it. If the user later messages
the agent on Telegram, the agent has no memory of what was already sent via cron.

In Hermes, `mirror.py` writes a `mirror=True` record directly to the target session's
transcript with `role: "assistant"`. The agent sees it as its own prior output on the
next turn.

---

## Participant ID Format

Before implementing the lookup, verify the actual format stored in `sessions.participants`
for each channel:

```sql
-- Run on Pi against live data
SELECT channel, participants FROM sessions LIMIT 10;
```

From channel adapter source code (`channels/src/drivers/telegram.ts`):
- `user_id = msg.from?.id?.toString()` — bare numeric string, e.g. `"123456789"`
- `chat_id` is stored separately in message `context` JSON, not in `participants`
- `sessions.participants = [user_id]` — bare numeric string

Therefore `participant_id = chat_id.to_string()` only works for **Telegram DMs** where
`chat_id == user_id`. For other channels, implementation must verify the format.

---

## Architecture

### 1. DB Migration

```sql
-- migrations/043_messages_is_mirror.sql
ALTER TABLE messages
  ADD COLUMN is_mirror BOOLEAN NOT NULL DEFAULT false;
```

No additional index needed — read as part of normal `load_messages` sequential scan.

Note: `sessions.participants` is a `TEXT[]` column. If performance becomes an issue with
many sessions, add `CREATE INDEX idx_sessions_participants ON sessions USING GIN (participants)`.
Not required for initial implementation.

### 2. `mirror_to_session` helper

New async function in `crates/hydeclaw-db/src/sessions.rs`:

```rust
pub async fn mirror_to_session(
    db: &PgPool,
    agent_id: &str,
    channel: &str,
    participant_id: &str,   // bare user_id as stored in sessions.participants
    text: &str,
) -> Result<bool>
```

**Logic:**

1. Find the most recent DM session for this agent + channel + participant.
   Excludes `per-chat` group sessions (user_id = `"*"`) — groups have no single
   recipient and should not be mirrored:

   ```sql
   SELECT id FROM sessions
   WHERE agent_id    = $1
     AND channel     = $2
     AND $3          = ANY(participants)
     AND user_id    != '*'
   ORDER BY started_at DESC
   LIMIT 1
   ```

2. If not found → return `Ok(false)` (silent miss — group chat, or no prior DM session).

3. If found → insert mirror record with `agent_id` set to identify the source agent:

   ```sql
   INSERT INTO messages (session_id, agent_id, role, content, is_mirror)
   VALUES ($1, $2, 'assistant', $3, true)
   ```

4. Return `Ok(true)`.

### 3. Hook in `scheduler/mod.rs`

After `engine.send_channel_message(ch, cid, &text)` succeeds (~line 962), fire-and-forget:

```rust
let db  = db.clone();
let aid = engine.cfg().agent.name.clone();
let ch  = ch.to_string();
let cid = cid.to_string();
let txt = text.to_string();
tokio::spawn(async move {
    // participant_id = bare chat_id string; equals user_id for Telegram DMs
    if let Err(e) = crate::db::sessions::mirror_to_session(
        &db, &aid, &ch, &cid, &txt,
    ).await {
        tracing::debug!(error = %e, "mirror_to_session failed (non-fatal)");
    }
});
```

The spawned task never panics into the caller — `send_channel_message` is unaffected.

### 4. `load_messages` — no change

Mirror records load as normal `role='assistant'` rows. The agent sees them as its own
prior output and can reference them on the next inbound turn. No filtering needed.

### 5. `MessageRow` — add `is_mirror` field

In `crates/hydeclaw-db/src/sessions.rs` (struct drives ts-rs codegen → `api.generated.ts`):

```rust
pub struct MessageRow {
    // ... existing fields ...
    #[sqlx(default)]
    pub is_mirror: bool,
}
```

`#[sqlx(default)]` ensures backward compatibility with queries that don't SELECT the column.
After adding the field, run `make gen-types` — `api.generated.ts` will auto-update.
`api.ts` re-exports `MessageRow` from `api.generated.ts` (line 56), no manual edit needed.

---

## Frontend

### Types

After `make gen-types`, `api.generated.ts` will include `is_mirror: boolean` on `MessageRow`.

In `chat-types.ts`, propagate to `ChatMessage` (the primary message type rendered by
`ChatThread`) — not `MessagePart`, which is for individual content blocks:

```typescript
export interface ChatMessage {
  // ... existing fields ...
  is_mirror?: boolean;
}
```

### Render

In the message component that renders assistant messages, add an inline badge when
`message.is_mirror === true`:

```tsx
{message.is_mirror && (
  <span className="text-[10px] text-orange-500 ml-1">↩ cron</span>
)}
```

Same style as `end_reason='compression'` marker in `CompactChainBanner`.

### Session list preview

Mirror messages are excluded from the "last message" preview in the session list.
Add `AND is_mirror = false` to the last-message subquery in `api_list_sessions`
(`gateway/handlers/sessions.rs`, the `SELECT ... ORDER BY created_at DESC LIMIT 1`
inner query used to compute the preview text).

---

## Error Handling

| Scenario | Behaviour |
|---|---|
| No matching session (group, no prior DM) | `Ok(false)`, debug-log, silent |
| DB find succeeds, INSERT fails | logged in spawned task, never surfaces to caller |
| `send_channel_message` fails | mirror never attempted (not reached) |
| Duplicate mirrors | harmless — agent sees repeated assistant text |

Mirror always happens after delivery — it never blocks or delays the channel send.

---

## Scope

**In scope:**
- Telegram DM deliveries (`send_channel_message` in cron delivery loop)
- Heartbeat jobs (same delivery path)
- Watchdog alerts (same delivery path)

**Not mirrored (by design):**
- Group chats (`per-chat` scope: `user_id="*"`) — no single recipient session
- Inbound messages — already in session natively
- Cross-agent mirroring (agent A → agent B's session)
- Retroactive mirror for past deliveries

---

## Testing

**Rust integration tests** (require `DATABASE_URL`, skip otherwise):

1. `mirror_to_session` finds DM session by participant → inserts row with `is_mirror=true`,
   `agent_id` set, `role='assistant'`
2. `mirror_to_session` with no matching session → returns `Ok(false)`, no insert
3. `mirror_to_session` DB insert fails (mock or constraint violation) → returns `Err`,
   caller's `tokio::spawn` logs and swallows

**Vitest** (`ui/src/__tests__/session-mirroring.test.tsx`):

1. Message with `is_mirror=true` renders "↩ cron" badge
2. Message with `is_mirror=false` renders no badge

---

## File Map

| File | Action |
|---|---|
| `migrations/043_messages_is_mirror.sql` | CREATE — add `is_mirror` column |
| `crates/hydeclaw-db/src/sessions.rs` | MODIFY — `MessageRow.is_mirror` + `mirror_to_session()` |
| `crates/hydeclaw-core/src/scheduler/mod.rs` | MODIFY — `tokio::spawn` mirror after `send_channel_message` |
| `ui/src/types/api.generated.ts` | MODIFY (via `make gen-types`) — `is_mirror: boolean` on `MessageRow` |
| `ui/src/stores/chat-types.ts` | MODIFY — `is_mirror?: boolean` on `ChatMessage` |
| `ui/src/components/chat/` | MODIFY — render "↩ cron" badge |
| `crates/hydeclaw-core/src/gateway/handlers/sessions.rs` | MODIFY — exclude mirrors from last-message preview |
| `ui/src/__tests__/session-mirroring.test.tsx` | CREATE — Vitest coverage |

---

## History

- **2026-05-03** — Design created via `/superpowers:brainstorming`.
  Reference: `D:/GIT/hermes-agent/gateway/mirror.py`.
