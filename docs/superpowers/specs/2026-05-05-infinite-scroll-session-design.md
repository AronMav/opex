# Infinite Scroll Session — Design Spec

**Date:** 2026-05-05
**Status:** Approved (v2 — issues fixed after spec review)

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
- Showing original pre-compression messages in the UI (they stay in DB for audit,
  but the API filters them out; only a divider marker is rendered)
- Configurable divider content or expandable summary view

---

## Architecture

### What Is Removed

- `maybe_split_session()` in `pipeline/bootstrap.rs` — chain creation logic
- `pending_split: bool` from `CompressorState`
- Child session creation on compression (no more `parent_session_id` chain building)

### What Is Added

- `compressed BOOLEAN` column on `messages` — marks messages replaced by compression
- `compression` event type in `session_events` WAL — records boundary and summary
  for API-level divider rendering (NOT used by bootstrap)
- Backward pagination on the messages API — `?before_id=&limit=50`
- `segment_count` field in session DTO
- `CompressionDivider` UI component
- Backward scroll detection via `IntersectionObserver`

### Compression Flow (New)

1. Compressor decides to compress (threshold exceeded, anti-thrash allows)
2. Messages in the middle range are marked `compressed = TRUE` in DB via batch UPDATE
3. A `session_events` record of type `compression` is inserted:

```json
{
  "type": "compression",
  "segment_index": 1,
  "summary": "...",
  "first_compressed_message_id": "uuid",
  "first_live_message_id": "uuid",
  "tokens_before": 45000,
  "tokens_after": 12000
}
```

`first_live_message_id` is the ID of the first non-compressed message after the
gap — this is what the frontend uses to position the divider in the rendered list.

4. `compaction_state` on the session is updated as before (same fields minus
   `pending_split`). Session continues. Bootstrap on next entry reads
   `compaction_state` to rebuild LLM context from summary + tail — unchanged.

Anti-thrash behavior is unchanged: if `ineffective_count` exceeds the threshold,
compression is skipped and a warning is logged. No split, no consequence beyond skipping.

### Source-of-Truth Separation

| Purpose | Source |
| --- | --- |
| LLM context rebuild (bootstrap) | `sessions.compaction_state` JSON |
| UI divider positioning | `session_events` compression records |
| Segment count badge | `COUNT(session_events WHERE type='compression')` |
| Anti-thrash state | `sessions.compaction_state` JSON |

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

`segment_index` for a new compression event = `compression_count` at the time
of the compression (before incrementing).

### session_events compression record

| Field | Type | Description |
| --- | --- | --- |
| `type` | `"compression"` | Event type |
| `segment_index` | `u32` | 1-based; equals `compression_count` before this compression |
| `summary` | `String` | LLM-generated summary of compressed range |
| `first_compressed_message_id` | `UUID` | First message marked `compressed=true` |
| `first_live_message_id` | `UUID` | First non-compressed message after the gap (divider anchor) |
| `tokens_before` | `i64` | Prompt tokens before compression |
| `tokens_after` | `i64` | Prompt tokens after compression |

---

## API

### Messages with Backward Pagination

**Existing endpoint extended:**

```
GET /api/sessions/{id}/messages?before_id={uuid}&limit=50
```

- `before_id` absent → returns latest 50 non-compressed messages (initial load)
- `before_id` present → returns 50 non-compressed messages older than that ID
- Compressed messages (`compressed = TRUE`) are **always filtered out** from results
- Response messages are ordered ASC by `created_at` (oldest first in array)

**Response:**

```json
{
  "messages": [...],
  "compression_events": [
    {
      "segment_index": 1,
      "first_live_message_id": "uuid",
      "summary": "..."
    }
  ],
  "has_more": true
}
```

`compression_events` contains only events whose `first_live_message_id` falls
within the returned message array. The frontend inserts a `CompressionDivider`
immediately before the message with that ID.

If the session has fewer than 50 non-compressed messages, all are returned and
`has_more = false`.

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
  // guard: if isLoadingHistory, return early
  // set isLoadingHistory = true
  // call GET /messages?before_id={firstMessageId}&limit=50
  // prepend messages to current array
  // insert CompressionDivider markers from compression_events
  // set hasMoreHistory from response.has_more
  // set isLoadingHistory = false
```

Initial load (`loadSession`): loads latest 50 non-compressed messages.
Sets `hasMoreHistory` from `has_more` field.

### Scroll Detection

`IntersectionObserver` on the first (topmost) message element in the chat list.
When it enters the viewport and `hasMoreHistory && !isLoadingHistory`:
→ calls `loadPreviousMessages()`.

After prepend, the observer is detached from the old first element and
re-attached to the new first element (via `useEffect` or ref callback that
runs after React re-render).

**Scroll position preservation:** the chat container uses `overflow-anchor: auto`
(CSS) which is supported natively in all modern browsers (Chromium, Firefox,
Safari 15.4+) — prepending elements does not cause a scroll jump. No manual
`scrollTop` manipulation needed.

### CompressionDivider Component

```
─────────── ◈ Контекст сжат · Сегмент 2 из 3 ───────────
```

- Thin horizontal rule, muted color (`text-muted-foreground`)
- Non-interactive (no click, no expand)
- Rendered immediately before the message with `id === first_live_message_id`
- Segment index from `compression_events[].segment_index`
- Total count from session's `segment_count`

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
- Compression event whose `first_live_message_id` is not found in the returned
  messages: skip divider silently (defensive — should not occur in practice).
- Bootstrap with no `session_events` compression records: behaves as today
  (compaction_state has no previous_summary → normal context build from all messages).

---

## Testing

- Unit: `CompressorState` serialization with `pending_split` absent
- Unit: `compress_messages()` — marks correct message IDs `compressed=true`,
  inserts WAL event with correct `first_compressed_message_id`,
  `first_live_message_id`, and token counts
- Unit: messages API — `before_id` pagination filters out compressed messages,
  returns ASC-ordered results, `has_more` correct
- Unit: messages API — `compression_events` injected only for events whose
  `first_live_message_id` falls within the returned page
- Unit: `segment_count` query returns correct count per session
- Integration: full compress cycle in single session — `session_events` has one
  compression record, messages have correct `compressed=true` flags, subsequent
  bootstrap reads `compaction_state` (not `session_events`) and rebuilds context
- UI: `CompressionDivider` renders before `first_live_message_id` message after
  `loadPreviousMessages()`
- UI: `IntersectionObserver` re-attaches to new first element after prepend
