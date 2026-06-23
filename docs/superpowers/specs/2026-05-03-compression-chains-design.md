# Compression Chains (P1.1) — Design

> **Source:** Hermes Agent P1.1 insight (`hermes-insights-plan.md`), extended with full chain UI.
> **Goal:** When trajectory compression fires, split into a new child session (B) linked to the parent (A) via `parent_session_id`. Expose the full chain via API and UI so users can navigate compression history.
> **Approach:** Bootstrap-time lazy split — session B is created only when the user sends the next message, using data already in DB + compaction_state. UI shows chain badge in session list + collapsible chain panel in chat header.

---

## Context

OPEX already has:
- **In-place compression** (`agent/compressor.rs` + `history.rs::compress_messages`) — fires in `execute.rs` before each LLM call when `compressor.should_compress()` returns true
- **Compaction state** (`sessions.compaction_state JSONB`, migration 040) — stores `previous_summary`, `ineffective_count`, `compression_count`
- **Message-level branching** (`parent_message_id`, `branch_from_message_id`, migration 012) — not the same as session-level chains

Missing: `parent_session_id`, `end_reason` on sessions, and the split logic itself.

Note: Hermes does NOT actually split sessions on compression — `trajectory_compressor.py` is an offline tool. This feature is a OPEX-specific improvement inspired by the Hermes P1.1 concept.

---

## Architecture

### Split timing: bootstrap.rs (lazy, start of next turn)

When compression fires in `execute.rs`:
1. `compress_messages()` modifies in-memory `messages` Vec
2. `compressor.pending_split = true` is set
3. `finalize.rs` saves `compaction_state` (with `pending_split=true`) to session A

On the user's **next message** to session A:
1. `bootstrap.rs` loads `compaction_state` → detects `pending_split=true`
2. Calls `maybe_split_session()` → creates session B, marks A
3. Pipeline continues with session B's ID
4. `data-session-id` SSE event naturally delivers B's ID to UI

**Why bootstrap (not finalize, not execute):**
- **Lazy**: session B is never created if the conversation is abandoned
- **No SSE changes**: `data-session-id` in the next turn naturally carries B's ID
- **Data available**: `previous_summary` and DB messages of A are accessible at bootstrap time
- **Clean boundary**: split happens at the natural turn boundary (new user input)

---

## DB Changes

### Migration: `041_sessions_compression_chains.sql`

```sql
ALTER TABLE sessions
  ADD COLUMN IF NOT EXISTS parent_session_id UUID REFERENCES sessions(id) NULL,
  ADD COLUMN IF NOT EXISTS end_reason        TEXT NULL;

COMMENT ON COLUMN sessions.parent_session_id IS
  'For compression chains: UUID of the session this was split from. NULL = root session.';
COMMENT ON COLUMN sessions.end_reason IS
  'Why this session ended: ''compression'' = split into child session. NULL = active or normal end.';

CREATE INDEX IF NOT EXISTS idx_sessions_parent_id
  ON sessions(parent_session_id)
  WHERE parent_session_id IS NOT NULL;
```

`end_reason` values: `'compression'` (split into child), `NULL` (active or normally completed). Extensible for future values (`'user_exit'`, etc.) without migration.

---

## Backend Changes

### 1. `CompressorState` — add `pending_split` field

**File:** `crates/opex-core/src/agent/compressor.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressorState {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub compression_count: u32,
    #[serde(default)]
    pub pending_split: bool,   // NEW — set true when compression fires, cleared in bootstrap
}
```

`#[serde(default)]` ensures backward-compatibility: existing `compaction_state` JSON without this field deserializes with `pending_split=false`.

In `Compressor`: add `pub pending_split: bool` field (default `false`), set to `true` in `record_compression_result()` **only when compression was effective** (savings >= `anti_thrash_min_savings`). Ineffective compressions (< threshold) do not trigger a split — the sessions would be nearly identical. Propagate through `to_json()` / `load()`.

### 2. `db/sessions.rs` — two new helpers

**File:** `crates/opex-db/src/sessions.rs`

```rust
/// Create a child session in a compression chain.
pub async fn create_chain_session(
    db: &PgPool,
    parent_id: Uuid,
    agent_id: &str,
    user_id: &str,
    channel: &str,
    title: Option<&str>,
) -> Result<Uuid>
// INSERT INTO sessions (parent_session_id, agent_id, user_id, channel, title, ...)
// returns new session UUID

/// Mark a session as ended for a specific reason.
pub async fn set_session_end_reason(
    db: &PgPool,
    session_id: Uuid,
    end_reason: &str,
) -> Result<()>
// UPDATE sessions SET end_reason = $1 WHERE id = $2
```

### 3. `history.rs` — `build_compressed_seed`

**File:** `crates/opex-core/src/agent/history.rs`

Pure function — no I/O, no LLM.

```rust
/// Build the initial message list for a chain child session.
/// Returns: [system_msg, summary_msg, ...tail_msgs]
/// The summary_msg role is chosen to alternate correctly with the last head message.
pub fn build_compressed_seed(
    system_msg: Option<&Message>,
    summary: &str,
    tail: &[Message],
) -> Vec<Message>
```

`summary` is wrapped in `SUMMARY_PREFIX` (already defined in `history.rs`). If `summary` is empty, uses the existing static fallback text from `compress_messages`. `system_msg` is `None` when the session had no system message (rare edge case — seed starts with summary directly).

The summary message role is `MessageRole::Assistant` — since the only head message is `system`, the next role must alternate to `assistant`. This is simpler than the role-detection logic in `compress_messages` (which handles arbitrary head sequences).

### 4. `pipeline/bootstrap.rs` — `maybe_split_session`

**File:** `crates/opex-core/src/agent/pipeline/bootstrap.rs`

```rust
/// If compaction_state has pending_split=true, create child session and return its ID.
/// Returns Ok(Some(child_id)) on success, Ok(None) if no split needed.
/// On DB error: logs warn, clears pending_split, returns Ok(None) — fail-open.
async fn maybe_split_session(
    db: &PgPool,
    session_id: Uuid,          // current (parent) session
    compressor: &mut Compressor,
    preserve_last_n: usize,
    agent_id: &str,
    user_id: &str,
    channel: &str,
) -> Result<Option<Uuid>>
```

**Logic:**
1. `if !compressor.pending_split { return Ok(None); }`
2. Load system message: `SELECT * FROM messages WHERE session_id=$1 AND role='system' LIMIT 1`
3. Load tail: `SELECT * FROM messages WHERE session_id=$1 ORDER BY created_at ASC` — all non-system messages, then take last `preserve_last_n` (chronological order; DO NOT use DESC or the tail will be reversed)
4. `let summary = compressor.previous_summary.as_deref().unwrap_or(STATIC_FALLBACK);` then `build_compressed_seed(system_msg, summary, &tail)`
5. `create_chain_session(db, session_id, agent_id, ...)` → child_id
6. Insert seed messages into child session with `session_id = child_id` (batch INSERT, preserving the order returned by `build_compressed_seed`)
7. `set_session_end_reason(db, session_id, "compression")`
8. `compressor.pending_split = false`
9. `set_compaction_state(db, child_id, compressor.to_json())`
10. Return `Ok(Some(child_id))`

On any error between steps 5–9: log warn, clear `pending_split`, return `Ok(None)` (fail-open — pipeline continues in original session).

Called in `bootstrap::handle()` after compaction_state is loaded, before messages are loaded for the pipeline.

### 5. `pipeline/finalize.rs` — no logic change

`set_compaction_state` already saves whatever is in `compressor.to_json()`. Since step 4 adds `pending_split` to `CompressorState`, finalize automatically persists it. No code changes needed in finalize.

### 6. New API endpoint: `GET /api/sessions/:id/chain`

**File:** `crates/opex-core/src/gateway/handlers/sessions.rs`

```rust
pub(crate) async fn api_session_chain(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse
```

DB query (recursive CTE, depth-limited):

```sql
WITH RECURSIVE chain AS (
  SELECT id, parent_session_id, end_reason, title, started_at, agent_id, 1 AS depth
  FROM sessions WHERE id = $1
  UNION ALL
  SELECT s.id, s.parent_session_id, s.end_reason, s.title, s.started_at, s.agent_id, c.depth + 1
  FROM sessions s
  JOIN chain c ON s.id = c.parent_session_id
  WHERE c.depth < 20
)
SELECT * FROM chain ORDER BY depth DESC;
```

Response (ordered root → current):
```json
{
  "chain": [
    { "id": "uuid-A", "title": "...", "end_reason": "compression", "parent_session_id": null, "depth": 2 },
    { "id": "uuid-B", "title": "...", "end_reason": "compression", "parent_session_id": "uuid-A", "depth": 1 },
    { "id": "uuid-C", "title": "...", "end_reason": null,          "parent_session_id": "uuid-B", "depth": 0 }
  ]
}
```

`depth=0` = the queried session (current). `depth=N` = root ancestor.

Also: `GET /api/sessions` list response adds `parent_session_id: string | null` and `end_reason: string | null` to each session row (needed for `ParentBadge` without extra requests).

Route registered as: `.route("/api/sessions/{id}/chain", get(api_session_chain))`

---

## Frontend Changes

### Types (`ui/src/types/api.ts` and `api.generated.ts`)

```typescript
// Add to SessionRow:
parent_session_id: string | null;
end_reason: string | null;

// New type:
export interface SessionChainEntry {
  id: string;
  title: string | null;
  end_reason: string | null;
  parent_session_id: string | null;
  depth: number;
  started_at: string;
  agent_id: string;
}

export interface SessionChainResponse {
  chain: SessionChainEntry[];
}
```

### Hook: `useSessionChain`

**File:** `ui/src/lib/queries.ts` (or `hooks/useSessionChain.ts`)

```typescript
export function useSessionChain(sessionId: string | null) {
  return useQuery({
    queryKey: qk.sessionChain(sessionId!),
    queryFn: () => apiGet<SessionChainResponse>(`/api/sessions/${sessionId}/chain`),
    enabled: !!sessionId,
    staleTime: 30_000,
  });
}
```

Query key: `qk.sessionChain(id)` = `["sessions", id, "chain"]`.

Only fetches when `sessionId` is non-null. Stale after 30s (chain rarely changes mid-session).

### Component: `ParentBadge`

**File:** `ui/src/components/chat/ParentBadge.tsx`

Small inline badge rendered under the session title in `SessionList`:

```tsx
// Props: parentSessionId: string, parentTitle: string | null, onNavigate: () => void
// Renders: "↩ от [title]" — click calls onNavigate
// Only rendered when parent_session_id != null
```

~15 lines. Uses `text-xs text-muted-foreground` styling. `onNavigate` sets the active session in chat-store.

### Component: `CompactChainBanner`

**File:** `ui/src/components/chat/CompactChainBanner.tsx`

Collapsible banner shown at the top of the chat area when `session.parent_session_id != null` (i.e. this session was split from a parent). Root sessions (A) do not show the banner — the chain API only traverses upward via `parent_session_id`, so A has no way to know it has children without a separate query.

```
┌──────────────────────────────────────────┐
│ 🗜 Цепочка компрессий  [свернуть ▲]      │
│                                          │
│  [A] "Разбор архитектуры..."   18:32  ↩  │
│  [B] "Разбор архитектуры... (2)" 19:05 ← │  ← current (bold)
│  [C] "Разбор архитектуры... (3)" 20:11   │
└──────────────────────────────────────────┘
```

- Data from `useSessionChain(activeSessionId)`
- Collapsed by default (localStorage persistence)
- Shown only when `chain.length > 1`
- Current session highlighted bold
- Click on any row → navigate to that session
- `↩` icon on sessions with `end_reason='compression'`

~60 lines.

### Integration points

- `SessionList` — render `<ParentBadge>` below session title when `session.parent_session_id != null`
- Chat page / `ChatLayout` — render `<CompactChainBanner>` above message list when `activeSession.parent_session_id != null` (i.e. root sessions do not show the banner)
- Invalidate `qk.sessionChain(sessionId)` in `queryClient` after `data-session-id` SSE event (chain may have grown)

---

## Error Handling

| Scenario | Behavior |
|---|---|
| DB error creating session B | Log warn, `pending_split=false`, continue in session A — fail-open |
| `previous_summary` empty | Use static fallback text (same as `compress_messages` today) |
| `chain` depth > 20 | CTE stops at depth=20; API returns chain up to that point |
| Session not found in chain API | 404 |
| UI chain fetch fails | Banner not shown; session works normally |

---

## Testing

### Rust unit tests (no DB)

**`history.rs` tests:**
1. `build_compressed_seed` with system + summary + 3 tail messages → correct order and roles
2. `build_compressed_seed` with `system_msg=None` → starts with summary message
3. `build_compressed_seed` with empty summary → uses fallback text

**`compressor.rs` tests:**
4. `pending_split` round-trips through `to_json()` / `load()`
5. `pending_split=false` deserialized correctly from old JSON without the field (`serde(default)`)
6. `record_compression_result` with effective compression (savings >= threshold) → `pending_split=true`; ineffective compression → `pending_split` unchanged (stays false)

### Rust integration tests (testcontainers Postgres)

**`tests/test_compression_chains.rs`:**
7. Full cycle: create session A → simulate compression (`pending_split=true` in compaction_state) → call `maybe_split_session` → session B created → A has `end_reason='compression'` → B has `parent_session_id=A` → B's messages match seed
8. `maybe_split_session` when `pending_split=false` → returns `None`, no DB writes
9. Chain API: A→B→C chain → `GET /chain` from C returns [A, B, C] ordered root-first
10. Chain depth guard: 21-level chain → API returns first 20, no infinite loop
11. Fail-open: DB error during child creation → `pending_split` cleared, original session ID returned

### Vitest (UI)

12. `useSessionChain` — mock fetch, verifies order root→current
13. `CompactChainBanner` — renders with 1 entry (not shown), 2 entries (shown), 3 entries (shown, current bold)
14. `CompactChainBanner` — collapsed state persisted to localStorage, restored on remount
15. `ParentBadge` — shown when `parent_session_id != null`, hidden when null
16. `ParentBadge` — `onNavigate` called on click

---

## Out of Scope

- Full history reconstruction (open parent session to read it — always available in DB)
- Undo / merge compression chains
- Automatic title suffix "(2)", "(3)" for child sessions — inherits parent title as-is
- `end_reason` values beyond `'compression'` (extensible without migration)
- Chain visualization beyond flat list (graph view, diff view)

---

## File Map

| File | Action |
|---|---|
| `migrations/041_sessions_compression_chains.sql` | CREATE — two new columns + index |
| `crates/opex-core/src/agent/compressor.rs` | MODIFY — add `pending_split` to `CompressorState` + `Compressor` |
| `crates/opex-db/src/sessions.rs` | MODIFY — add `create_chain_session`, `set_session_end_reason` |
| `crates/opex-core/src/agent/history.rs` | MODIFY — add `build_compressed_seed` |
| `crates/opex-core/src/agent/pipeline/bootstrap.rs` | MODIFY — add `maybe_split_session`, call it after compaction_state load |
| `crates/opex-core/src/gateway/handlers/sessions.rs` | MODIFY — add `api_session_chain` + route |
| `crates/opex-core/src/gateway/handlers/sessions.rs` | MODIFY — add `parent_session_id`, `end_reason` to session list DTO |
| `crates/opex-core/tests/test_compression_chains.rs` | CREATE — integration tests |
| `ui/src/types/api.ts` | MODIFY — extend `SessionRow`, add `SessionChainEntry`, `SessionChainResponse` |
| `ui/src/types/api.generated.ts` | MODIFY — add `parent_session_id`, `end_reason` if `SessionRow` is ts-rs generated |
| `ui/src/lib/queries.ts` | MODIFY — add `qk.sessionChain`, `useSessionChain` |
| `ui/src/components/chat/ParentBadge.tsx` | CREATE — inline badge for session list |
| `ui/src/components/chat/CompactChainBanner.tsx` | CREATE — collapsible chain panel in chat |
| `ui/src/app/(authenticated)/chat/` | MODIFY — render `CompactChainBanner` |
| `ui/src/app/(authenticated)/chat/page.tsx` | MODIFY — render `ParentBadge` in session list (line ~582) |
| `ui/src/__tests__/compression-chains.test.ts` | CREATE — vitest coverage |

---

## History

- **2026-05-03** — Design created via `/superpowers:brainstorming`. Full chain navigation (variant В) approved: DB fields + bootstrap split + chain API + session list badge + chat chain banner.
