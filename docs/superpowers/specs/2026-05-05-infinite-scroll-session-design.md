# Infinite Scroll Session — Design Spec

**Date:** 2026-05-05
**Status:** Approved

## Problem

When context compression triggers a chain split, a new child session is created
(`parent_session_id` link). The user sees Session A and Session B as separate
entries in the sidebar. There is no way to scroll through the full history as a
single continuous conversation.

## Goal

One logical session from the user's perspective. Scrolling up loads older
messages seamlessly. Compression boundaries are visible as thin dividers.
Chain split is removed entirely — no new sessions created on compression.

## Out of Scope

- Migrating existing chained sessions (old sessions remain as separate entries)
- Showing original pre-compression messages (only the summary is shown at the divider)
- Configurable divider content or expandable summary view

---

## Architecture

### What Is Removed

- `maybe_split_session()` in `pipeline/bootstrap.rs` — chain creation logic
- `pending_split: bool` from `CompressorState`
- Child session creation on compression (no more `parent_session_id` chain building)

### What Is Added

- `compressed BOOLEAN` column on `messages` — marks messages replaced by compression
- `compression` event type in `session_events` WAL — records summary and boundary
- Backward pagination on the messages API — `?before_id=&limit=50`
- `segment_count` field in session DTO
- `CompressionDivider` UI component
- Backward scroll detection via `IntersectionObserver`

### Compression Flow (New)

1. Compressor decides to compress (threshold exceeded, anti-thrash allows)
2. Messages in the middle range are marked `compressed = TRUE` in DB
3. A `session_events` record of type `compression` is inserted:
   ```json
   {
     "type": "compression",
     "segment_index": 1,
     "summary": "...",
     "first_compressed_message_id": "uuid",
     "last_compressed_message_id": "uuid",
     "tokens_before": 45000,
     "tokens_after": 12000
   }
   ```
4. Session continues. Bootstrap on next entry reads `session_events` to rebuild
   LLM context from summary + tail — same algorithm, single session.

Anti-thrash behavior is unchanged: if `ineffective_count` exceeds the threshold,
compression is skipped and a warning is logged. No split, no consequence beyond skipping.

---

## Data Model

### Migration

```sql
ALTER TABLE messages ADD COLUMN compressed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX idx_messages_compressed ON messages(session_id, compressed)
    WHERE compressed = TRUE;
```

No changes to `sessions` table schema. `segment_count` is computed on read.

### CompressorState (updated)

```rust
pub struct CompressorState {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub compression_count: u32,
    // pending_split removed
}
```

### session_events compression record

| Field | Type | Description |
| --- | --- | --- |
| `type` | `"compression"` | Event type |
| `segment_index` | `u32` | 1-based counter per session |
| `summary` | `String` | LLM-generated summary of compressed range |
| `first_compressed_message_id` | `UUID` | First message in compressed range |
| `last_compressed_message_id` | `UUID` | Last message in compressed range (divider anchor) |
| `tokens_before` | `i64` | Prompt tokens before compression |
| `tokens_after` | `i64` | Prompt tokens after compression |

---

## API

### Messages with Backward Pagination

**Existing endpoint extended:**

```
GET /api/sessions/{id}/messages?before_id={uuid}&limit=50
```

- `before_id` absent → returns latest 50 messages (initial load, unchanged)
- `before_id` present → returns 50 messages older than that ID (DESC by created_at, reversed)

**Response:**

```json
{
  "messages": [...],
  "compression_events": [
    {
      "segment_index": 1,
      "last_compressed_message_id": "uuid",
      "summary": "..."
    }
  ],
  "has_more": true
}
```

`compression_events` contains only events whose `last_compressed_message_id`
falls within the returned message range. Frontend inserts dividers accordingly.

### Session DTO — segment_count

`segment_count` added to the existing session response:

```json
{ "id": "...", "title": "...", "segment_count": 3 }
```

Computed via:
```sql
SELECT COUNT(*) FROM session_events
WHERE session_id = $1 AND type = 'compression'
```

Joined at query time when loading session lists and individual sessions.

---

## Frontend

### Chat Store Changes (`chat-store.ts`)

New state fields:
```ts
hasMoreHistory: boolean   // true if there are older messages above
isLoadingHistory: boolean // prevents concurrent loads
```

New action:
```ts
loadPreviousMessages(): void
  // calls GET /messages?before_id={firstMessageId}&limit=50
  // prepends messages to current array
  // inserts CompressionDivider markers from compression_events
  // sets hasMoreHistory from response.has_more
```

Initial load (`loadSession`): loads latest 50 messages. Sets `hasMoreHistory`
from `has_more` field.

### Scroll Detection

`IntersectionObserver` on the first (topmost) message element in the chat list.
When it enters the viewport and `hasMoreHistory && !isLoadingHistory`:
→ calls `loadPreviousMessages()`.

No scroll event listeners. No manual scroll position tracking.

### CompressionDivider Component

```
─────────── ◈ Контекст сжат · Сегмент 2 из 3 ───────────
```

- Thin horizontal rule, muted color (`text-muted-foreground`)
- Non-interactive (no click, no expand)
- Inserted between the last compressed message and the next live message
- Segment index comes from `compression_events[].segment_index`

### Session List Badge

In `SessionListItem`: if `segment_count > 1`, render a small inline badge
next to the session title:

```
My Long Session  ◈ 3
```

Badge uses `text-xs text-muted-foreground`. No tooltip, no click target.

---

## Error Handling

- `loadPreviousMessages()` failure: show toast error, reset `isLoadingHistory`.
  User can retry by scrolling up again.
- Compression event without matching message range: skip divider silently
  (defensive — should not occur in practice).
- Bootstrap with no `session_events` compression records: behaves as today
  (no summary injection, normal context build).

---

## Testing

- Unit: `CompressorState` serialization with `pending_split` absent (migration)
- Unit: `compress_messages()` — marks correct message IDs as compressed,
  inserts WAL event with correct boundary IDs and token counts
- Unit: messages API — `before_id` pagination returns correct range and
  injects compression_events only for events within the returned range
- Unit: `segment_count` query returns correct count per session
- Integration: full compress cycle in single session — session_events has
  one compression record, messages have correct `compressed=true` flags
- UI: `CompressionDivider` renders at correct position after `loadPreviousMessages()`
- UI: `IntersectionObserver` triggers load when first message enters viewport
