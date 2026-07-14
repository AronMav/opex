# Chat & Notifications Robustness — Design Spec

**Date:** 2026-07-14
**Status:** Approved design, pending implementation plans (one per phase)
**Scope:** Web-UI chat and notification subsystems + their backend contracts.

---

## 1. Motivation

Two architecture reviews (web chat; notifications + realtime WS) found that the
chat streaming stack is mature and largely best-practice, while the notification
stack is a solid but first-generation "single global feed" with real data-loss
and cross-tab-consistency gaps. This spec collects every agreed improvement into
one umbrella initiative, decomposed into six independently-implementable phases.

The system is **single-operator**: one global `notifications` table, one global
broadcast channel (`ui_event_tx`), one WebSocket endpoint (`/ws`) that every tab
connects to. All designs below preserve that model — there is no per-recipient
routing.

### Goals

- No lost notifications across WS disconnects or broadcast `Lagged` gaps.
- Consistent unread/read state across all open tabs.
- Full notification history (pagination) + per-type mute/sound/push preferences.
- Optional Web Push delivery for critical events when no tab is focused.
- Session-list pagination in the chat sidebar.
- Multi-tab coordination (single shared WS), offline send queue, and gated
  streaming performance polish.

### Non-goals (explicit)

- Rewriting the chat history↔live-overlay reconciliation model (weakness Ч7).
  It is standard (React Query history + live overlay) and battle-tested through
  many regressions; migration cost exceeds value.
- Per-recipient / assignment / @-mention notification model — single-operator.
- Editing assistant messages (only regenerate/fork, as today).

---

## 2. Current state (evidence anchors)

**Notifications backend**
- Table: `migrations/008_notifications.sql:5` — `id UUID PK (gen_random_uuid)`,
  `type TEXT`, `title`, `body`, `data JSONB`, `read BOOL`, `created_at TIMESTAMPTZ`.
  Index `notifications_read_created_at (read, created_at DESC)` (008:15). Partial
  unread index in `migrations/022_data_layer_indexes.sql:29`. **`id` is a UUID,
  not monotonic — cursors must be built on `created_at` with an `id` tiebreak.**
- DB layer: `crates/opex-db/src/notifications.rs` — `create_notification`,
  `list_notifications` (rows + unread count), `mark_read`, `mark_all_read`,
  `cleanup_old_notifications` (30-day retention).
- HTTP + `notify()` helper: `crates/opex-core/src/gateway/handlers/notifications.rs`
  (`notify()` at :148, broadcast at :165, fire-and-forget `.ok()`).
- Broadcast channel `ui_event_tx` capacity 512 (`main.rs:555`); WS forward loop
  `channel_ws/mod.rs:503-514`; `RecvError::Lagged` only logs (:508) → silent drop.

**Notifications frontend**
- WS client `ui/src/lib/ws.ts` (`WsManager`); global WS store
  `ui/src/stores/ws-store.ts`; subscription hook `ui/src/hooks/use-ws-subscription.ts`.
- Notification state `ui/src/stores/notification-store.ts`; RQ wiring
  `ui/src/lib/queries.ts:678-742`; bell UI `ui/src/components/notification-bell.tsx`
  (mounted `ui/src/components/app-sidebar.tsx:162`); connection lifecycle + global
  toasts `ui/src/app/(authenticated)/layout.tsx`.
- WS auth: one-time 30s ticket via `POST /api/auth/ws-ticket`
  (`handlers/auth.rs:16`), validated `middleware.rs:313-324`; `/ws` not
  loopback-exempt.

**Chat**
- Backend contract: `crates/opex-core/src/gateway/handlers/chat/{sse.rs,
  sse_converter.rs,resume.rs,streaming_db.rs,misc.rs}`. `POST /api/chat` accepts
  `user_message_id: Option<Uuid>` (`sse.rs:55`, passed through :173). Server-side
  `StreamRegistry` buffers every event with a monotonic per-session `seq` used as
  SSE `id:`; resume via `GET /api/chat/{id}/stream` with `Last-Event-ID`.
- Frontend streaming: `ui/src/stores/streaming-renderer.ts`, `stream-session.ts`,
  `stream/stream-processor.ts`, `stream/stream-reconnect.ts`, `stream/stream-buffer.ts`.
  Session list `useSessions` hard-codes `limit=40` (`queries.ts:502`), ignores
  `total`. Double throttle: 50ms store commit (`stream-session.ts:120`) + 40ms
  per-TextPart interpolation (`use-smoothed-text.ts:45-69`). Full re-lex per tick
  (`components/ui/markdown.tsx:245`). Silent SSE parse drop
  (`stream/stream-processor.ts:120-123`).

---

## 3. Cross-cutting design decisions

- **Cursor shape (recovery + pagination).** Because `notifications.id` is a UUID,
  all cursors are the composite `(created_at, id)`. "Newer than" =
  `created_at > $since OR (created_at = $since AND id > $sinceId)`. "Older than"
  (history) = the symmetric `<`. Ordering is always `created_at DESC, id DESC`.
- **New WS event types** (additive to the existing `/ws` union in
  `ui/src/types/ws.ts`): `notification_read`, `notifications_read_all`,
  `notifications_cleared`. Payloads carry only ids / no body — clients reconcile
  from their local cache and the authoritative `unread_count`.
- **Server is authoritative for `unread_count`.** Every list/page response and
  every read/clear WS event carries the current `unread_count`; the client stops
  doing blind local decrements.
- **Preferences gate `notify()` centrally.** All suppression (mute/sound/push
  selection) happens server-side at the single `notify()` choke point so every
  trigger site inherits it for free.

---

## 4. Phase 1 — Notification reliability (recovery + cross-tab read)

### 4.1 N1 — Missed-event recovery

**Backend.** Extend `GET /api/notifications` with optional `since` (RFC3339
timestamp) + `sinceId` (UUID). When present, return notifications strictly newer
than the composite cursor (capped, e.g. 200), ordered `created_at DESC, id DESC`,
plus the current `unread_count`. Absent → existing behaviour (first page).

**Frontend.** `WsManager` exposes a reconnect/open signal (callback or store flag).
The notification sync layer tracks a `highWater = (created_at, id)` of the newest
item it has applied. It refetches `?since=&sinceId=` and merges (dedup by id) on:
1. WS `onopen` after any (re)connect,
2. `visibilitychange` → visible (tab focus),
3. a 60s low-frequency safety-net interval while visible.

Triggers (1)+(3) together cover both a dropped socket and a silent broadcast
`Lagged` (which does not disconnect the socket). No change to the server
`Lagged` handling is required.

### 4.2 N2 — Cross-tab read-state sync (+ N5 fix)

**Backend.** After a successful state change, broadcast to `ui_event_tx`:
- `mark_read` → `{"type":"notification_read","data":{"id":<uuid>,"unread_count":<n>}}`
- `mark_all_read` → `{"type":"notifications_read_all","data":{"unread_count":0}}`
- clear-all → `{"type":"notifications_cleared"}`

`mark_read` already reports whether a row actually changed
(`opex-db/notifications.rs:94`) — only broadcast + recompute when it did.

**Frontend.** `WsManager` dispatches the new events; the notification layer marks
the matching item(s) read and sets `unread_count` from the event payload (server
truth). This eliminates the blind `unread_count - 1` drift (N5). All tabs converge.

### 4.3 N4/N7 — Quick UX fixes

- **N4 (sound on load).** Drive the beep + flash from the WS `notification`
  dispatch (a genuine new arrival) instead of a `unread_count` delta effect. Guard
  with an `initialized` flag so the initial history fetch never triggers sound.
- **N7 (toast↔bell dedup).** `approval_requested` currently appears both as a 30s
  sonner toast (`layout.tsx:102-124`) and as an independent `tool_approval` bell
  row (`approval_manager.rs:133`). Link them by shared `approval_id`: play sound
  once, keep the toast as the transient actionable surface and the bell row as the
  persistent record, and reflect resolution in both via `approval_resolved`.

### 4.4 Testing (Phase 1)

- Backend: cursor query returns exactly the newer set across a same-timestamp
  tiebreak; read/clear broadcasts fire only on real change; `unread_count`
  correctness.
- Frontend: simulated WS drop → reconnect refetch fills the gap; read in tab A
  updates tab B (two-client test); no sound on cold load; approval toast+bell
  produce one sound and reconcile on resolve.

---

## 5. Phase 2 — Notification pagination & history (N3)

**Backend.** Extend `GET /api/notifications` with history cursor `before` +
`beforeId` + `limit` (clamp 1–200), symmetric to N1's `since`. Every page response
carries `unread_count`.

**Frontend.** The bell dropdown list becomes a `useInfiniteQuery` keyed on the
history cursor, with infinite-scroll / "load more". **Source-of-truth shift:** the
list moves into the React Query infinite cache; live `notification` WS events
prepend into the first page via `queryClient.setQueryData`; read/clear events patch
the cache. The zustand `notification-store` shrinks to only `unread_count` +
ephemeral flash state, removing the current dual-state ambiguity.

**Testing.** Infinite scroll reaches items older than the first page; a live
arrival during scroll prepends without duplicating or breaking pagination; read
state patches the correct cached item.

---

## 6. Phase 3 — Preferences & mute

**Data model.** New migration:

```sql
CREATE TABLE notification_prefs (
    type       TEXT        PRIMARY KEY,
    muted      BOOLEAN     NOT NULL DEFAULT FALSE,
    push       BOOLEAN     NOT NULL DEFAULT FALSE,
    sound      BOOLEAN     NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

Global (single-operator). A missing row = defaults (`muted=false, push=false,
sound=true`). No pre-seeding required.

**Backend.**
- `GET /api/notifications/prefs` → all rows (merged with defaults for known types).
- `PUT /api/notifications/prefs` → upsert; allowed keys `muted`, `push`, `sound`
  per `type` (validate `type` against the known set).
- `notify()` reads prefs for the event type:
  - `muted` → **still persist the row** (history/audit preserved) but skip the WS
    broadcast, sound, and push. It surfaces in history on next fetch/open.
  - `sound=false` → broadcast normally but the client suppresses the beep for that
    type (send the effective `sound` flag in the notification payload, or let the
    client read prefs — payload flag preferred to avoid a client round-trip).

**Frontend.** A gear in the bell dropdown header opens a preferences panel: per-type
toggles (mute / sound / push), a global sound switch, and an "Enable desktop
notifications" button that bridges to Phase 5 (Web Push subscribe). Reuse the panel
component in a Settings route if one exists.

**Testing.** Muted type → row exists in DB + history, no WS frame observed, no
sound; `sound=false` type → frame present, no beep; prefs round-trip via PUT/GET.

---

## 7. Phase 4 — Chat session-list pagination (C3)

**Backend.** Session list endpoint already returns `total`. Add a stable cursor
(`before`/`beforeId` on `updated_at` + id, or offset) for paging.

**Frontend.** `useSessions` (`queries.ts:502`) → `useInfiniteQuery`, page size 40,
infinite-scroll in the sidebar, using `total` to stop. Isolated and low-risk.

**Testing.** Sidebar reveals the (41+)th session via scroll; no duplication at page
boundaries; active-session highlighting survives paging.

---

## 8. Phase 5 — Web Push (behind config flag)

**Gating.** New `[notifications.push]` config: `enabled=false`,
`vapid_public_key`, `vapid_private_key`, `subject` (mailto). If `enabled=false` or
keys unset → the feature is fully inert (endpoints return 404/disabled, `notify()`
never attempts push). Default off.

**Data model.** New migration:

```sql
CREATE TABLE push_subscriptions (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    endpoint   TEXT        NOT NULL UNIQUE,
    p256dh     TEXT        NOT NULL,
    auth       TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Backend endpoints.**
- `GET /api/notifications/push/pubkey` → VAPID public key (base64url).
- `POST /api/notifications/push/subscribe` → store `{endpoint, keys{p256dh, auth}}`
  (upsert on `endpoint`, bump `last_seen`).
- `POST /api/notifications/push/unsubscribe` → delete by `endpoint`.

**Delivery.** In `notify()`, after persist and the mute check: if push is enabled
AND `prefs[type].push` AND `type` ∈ default critical set
(`tool_approval`/`approval_requested`, `agent_error`, `watchdog_alert`,
`infra_decision`) → send a Web Push to every stored subscription. On a `410 Gone`
/ `404` response, delete that subscription (standard pruning).

**Crypto — the key risk (rustls-only, no OpenSSL).** Web Push requires VAPID
ES256 signing (ECDSA P-256) and RFC 8291 payload encryption (`aes128gcm` content
encoding: ECDH P-256 → HKDF-SHA256 → AES-128-GCM). Implement on the pure-Rust
**RustCrypto** stack (`p256`, `hkdf`, `sha2`, `aes-gcm`, `base64`) with `reqwest`
(rustls) for transport. **Do not** pull in any OpenSSL-backed `web-push` crate.
This is a dedicated task with unit tests against the RFC 8291 / RFC 8292 test
vectors before wiring into `notify()`.

**Frontend.**
- `ui/public/sw.js` service worker: `push` event → `showNotification(title, {body,
  data, tag})`; `notificationclick` → focus an existing client or open the route
  derived from `data` (reuse `getNotificationRoute`).
- Registration + subscribe flow behind the Phase 3 "Enable desktop notifications"
  button: `Notification.requestPermission()` → `registration.pushManager.subscribe(
  {userVisibleOnly:true, applicationServerKey:<pubkey>})` → POST subscribe. Handle
  permission-denied and unsupported-browser gracefully.
- Verify the service worker is served at `/sw.js` under Next.js static export.

**Testing.** RustCrypto encryption unit vectors pass; subscribe/unsubscribe
round-trip; a critical notification with push-enabled prefs reaches a subscribed
(background) client; `410` prunes the subscription; feature inert when flag off.

---

## 9. Phase 6 — Multi-tab, outbox, performance

Three independent blocks; may become three separate plans.

### 9.1 C1 — Multi-tab leader election

- **Leader.** Use the Web Locks API: the leader tab holds an exclusive lock
  `opex-ws-leader` (a never-resolving `navigator.locks.request`); when it closes,
  the lock releases and another tab acquires it. The leader owns the single
  WebSocket.
- **Relay.** Leader forwards inbound WS events to other tabs via a
  `BroadcastChannel("opex-tabs")`; non-leader tabs send outbound requests
  (`subscribe_logs`, ping, etc.) to the leader over the same channel.
- **Shared state.** `activeSession`, drafts, read-state, and `ctx_limit:*` sync via
  BroadcastChannel instead of racing on `localStorage` (fixes W1 last-writer-wins).
- **Chat streams stay per-tab**; cross-tab display of an in-flight reply relies on
  the existing resume path (a second tab with the same session open resumes the
  buffered stream). Leader election governs the WS/event plane, not the SSE plane.
- **Fallback.** Browsers without Web Locks fall back to today's per-tab WS (feature
  degrades, does not break).

### 9.2 C2 — Offline outbox

- Per-session queue of unsent user messages persisted to IndexedDB. On a network
  send failure, the optimistic bubble becomes `queued` (not just `failed`), with a
  manual retry affordance.
- On `online` / WS reconnect, flush the queue in order via `startStream`.
- **Idempotency:** each queued message keeps its pre-allocated `user_message_id`;
  retries reuse it. **Requirement:** server-side persist must dedup/upsert on
  `user_message_id` so a retry after a partially-successful send cannot create a
  duplicate user message (verify current behaviour; add dedup if missing).

### 9.3 Performance (gated — "only on lag complaints" + benchmark gate)

- **Ч6 (low-risk, do now).** When `parseSseEvent` returns `null`
  (`stream-processor.ts:120-123`), increment a counter and surface a telemetry
  signal (and a subtle user indicator on repeated drops) instead of a silent
  `console.warn`. Catches Rust↔client SSE schema drift.
- **Ч5 (gated).** Collapse the double throttle (50ms store commit + 40ms
  per-TextPart interpolation) into a single mechanism. Land only behind a
  streaming-latency benchmark that shows no regression.
- **Ч4 (gated).** Re-lex only the growing tail block rather than the full
  accumulated text each tick. Same benchmark gate.

Each gated item requires a before/after streaming benchmark; the stable chat hot
path is not touched without a measured reason.

---

## 10. Rollout order & risk

| Phase | Value / cost | Risk |
| --- | --- | --- |
| 1 Reliability | Highest / low | Low — additive endpoints + WS events |
| 2 Pagination/history | High / medium | Medium — notification state SoT shift to RQ |
| 3 Prefs & mute | High / low | Low — new table + `notify()` gate |
| 4 Session pagination | Medium / low | Low — isolated frontend |
| 5 Web Push | Medium / high | **High — pure-Rust crypto**, SW infra |
| 6 Multi-tab/outbox/perf | Medium / high | Medium–high — hot-path & tab coordination |

Implement in numeric order. Each phase gets its own implementation plan via the
writing-plans skill; do not batch phases into one plan.

---

## 11. Open items to verify during implementation

- Exact session-list endpoint + its current cursor/`total` shape (Phase 4).
- Whether `POST /api/chat` persist already dedups on `user_message_id` (Phase 6.2);
  add upsert-on-conflict if not.
- Next.js static-export service-worker serving path and scope (Phase 5).
- A RustCrypto Web Push reference/test-vector source before implementing 8-crypto.
