# Notifications Phase 1 — Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Web-UI notifications survive WS disconnects (no lost alerts, correct unread badge), sync read-state across tabs, and stop the false/duplicate notification sound.

**Architecture:** Purely additive. The backend gains three new broadcast event `"type"` literals (`notification_read`, `notifications_read_all`, `notifications_cleared`) emitted from the existing read/clear handlers over the existing `ui_event_tx: broadcast::Sender<String>` bus — **no schema change, no migration**. The frontend gains (a) refetch-on-reconnect/focus/interval recovery via the existing `GET /api/notifications` capped list, (b) handlers for the three new events that reconcile the notification store to server-truth unread counts, and (c) a `newArrivalSeq` counter so sound/flash fire only on genuine live arrivals, not on refetch or cold load.

**Tech Stack:** Rust/Axum + sqlx (backend), TypeScript/React + Zustand + React Query + vitest (frontend).

## Global Constraints

- **No OpenSSL / rustls-only** — this phase adds no crates, so nothing to check.
- **No DB migration in Phase 1** — N2 reuses the existing `notifications` table. The `?since` cursor endpoint from the spec is deferred to Phase 2 (its natural home alongside pagination); Phase 1 achieves "no lost notifications" by refetching the capped (20-row) newest-first list, whose global `unread_count` COUNT is always accurate.
- **Broadcast bus is `tokio::sync::broadcast::Sender<String>`** carrying pre-serialized JSON strings — every event is `serde_json::json!({"type": "...", ...}).to_string()`, never a Rust enum.
- **Windows cannot run the Rust test binaries (they crash)** — per project convention, verify Rust changes locally with `make check` (compiles all targets) and run the actual Rust tests via `make test-db` on the server. Frontend vitest runs locally from `ui/`.
- **Frontend tests run only from `ui/`**: `cd ui && npm test -- <filter>` (vitest one-shot).
- **Commits go to `master`** (project convention: work directly in master, incremental). **Do NOT add any `Co-Authored-By` / Claude attribution** to commit messages.
- **No gen-types drift** — new WS event shapes are hand-authored in `ui/src/types/ws.ts`; no Rust `#[derive(ts_rs::TS)]` DTO changes, so no `api.generated.ts` regeneration.

---

## File Structure

**Backend (crate `opex-db`)**
- `crates/opex-db/src/notifications.rs` — add `count_unread(db) -> Result<i64>` (fresh unread count for broadcast payloads).

**Backend (crate `opex-core`)**
- `crates/opex-core/src/gateway/handlers/notifications.rs` — add three pure event-builder fns + wire the read/clear handlers to broadcast after a successful state change.

**Frontend**
- `ui/src/types/ws.ts` — add `WsNotificationRead`, `WsNotificationsReadAll`, `WsNotificationsCleared` interfaces + union members.
- `ui/src/stores/notification-store.ts` — add `newArrivalSeq`; add `applyRead`/`applyReadAll`/`applyCleared`/`resolveApproval` actions; fix `markRead` over-decrement; bump `newArrivalSeq` on genuine prepend.
- `ui/src/stores/notification-store.test.ts` — **create** vitest suite for the store logic.
- `ui/src/lib/queries.ts` — extend `useNotificationWsSync` (subscribe to the 3 new events + `approval_resolved`), add `useNotificationRecovery` (reconnect invalidate), add focus/interval refetch to `useNotifications`.
- `ui/src/components/notification-bell.tsx` — drive sound/flash from `newArrivalSeq`; mount `useNotificationRecovery`.

---

## Task 1: Backend — `count_unread` DB helper

**Files:**
- Modify: `crates/opex-db/src/notifications.rs` (add fn after `list_notifications`, ~line 84)
- Test: `crates/opex-db/src/notifications.rs` (inline `#[sqlx::test]`)

**Interfaces:**
- Produces: `pub async fn count_unread(db: &PgPool) -> anyhow::Result<i64>` — count of rows with `read = FALSE`. Consumed by Task 2's broadcast payloads.

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/opex-db/src/notifications.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn count_unread_counts_only_unread(pool: PgPool) -> Result<()> {
        create_notification(&pool, "agent_error", "a", "b", serde_json::json!({})).await?;
        let n2 = create_notification(&pool, "agent_error", "c", "d", serde_json::json!({})).await?;
        assert_eq!(count_unread(&pool).await?, 2);
        mark_read(&pool, n2.id).await?;
        assert_eq!(count_unread(&pool).await?, 1);
        Ok(())
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo check -p opex-db` (on Windows) — expect a **compile error**: `cannot find function 'count_unread' in this scope`.
(Full test execution: `make test-db` on the server — expected FAIL for the same reason until Step 3.)

- [ ] **Step 3: Write minimal implementation**

Insert after `list_notifications` (after line 84) in `crates/opex-db/src/notifications.rs`:

```rust
/// Count currently-unread notifications. Used to build cross-tab read-sync
/// broadcast payloads with a server-authoritative unread count.
pub async fn count_unread(db: &PgPool) -> Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read = FALSE")
        .fetch_one(db)
        .await?;
    Ok(n)
}
```

- [ ] **Step 4: Verify it compiles / passes**

Run: `make check`
Expected: no errors.
Then (server): `make test-db` → `count_unread_counts_only_unread` PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-db/src/notifications.rs
git commit -m "feat(notifications): count_unread helper for read-sync payloads"
```

---

## Task 2: Backend — broadcast read/clear events

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/notifications.rs` (add builders near top; edit `api_mark_notification_read` ~101-112, `api_mark_all_notifications_read` ~115-125, `api_clear_all_notifications` ~127-140)
- Test: `crates/opex-core/src/gateway/handlers/notifications.rs` (inline `#[cfg(test)]` for the pure builders)

**Interfaces:**
- Consumes: `crate::db::notifications::count_unread` (Task 1); the already-imported `ChannelBus` (holds `ui_event_tx`) and `InfraServices` (holds `db`).
- Produces: three WS events on the bus — `{"type":"notification_read","data":{"id":<uuid-str>,"unread_count":<i64>}}`, `{"type":"notifications_read_all","data":{"unread_count":<i64>}}`, `{"type":"notifications_cleared"}`. Consumed by Task 4's frontend handlers.

- [ ] **Step 1: Write the failing test**

Add at the bottom of `crates/opex-core/src/gateway/handlers/notifications.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_event_shape() {
        let id = Uuid::nil();
        let ev = notification_read_event(id, 3);
        assert_eq!(ev["type"], "notification_read");
        assert_eq!(ev["data"]["id"], id.to_string());
        assert_eq!(ev["data"]["unread_count"], 3);
    }

    #[test]
    fn read_all_event_shape() {
        let ev = notifications_read_all_event(0);
        assert_eq!(ev["type"], "notifications_read_all");
        assert_eq!(ev["data"]["unread_count"], 0);
    }

    #[test]
    fn cleared_event_shape() {
        let ev = notifications_cleared_event();
        assert_eq!(ev["type"], "notifications_cleared");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo check -p opex-core` (on Windows) — expect **compile error**: `cannot find function 'notification_read_event'`.
(Full execution deferred to `make test-db` on server.)

- [ ] **Step 3: Write minimal implementation — the builders**

Add these three module-level fns near the top of `crates/opex-core/src/gateway/handlers/notifications.rs` (after the imports, before `routes()`):

```rust
// ── Cross-tab read-sync broadcast events ─────────────────────────────────────
// Emitted over `ui_event_tx` so every open tab reconciles read-state to the
// server-authoritative unread count (fixes blind local decrement drift).

fn notification_read_event(id: Uuid, unread_count: i64) -> serde_json::Value {
    serde_json::json!({
        "type": "notification_read",
        "data": { "id": id.to_string(), "unread_count": unread_count }
    })
}

fn notifications_read_all_event(unread_count: i64) -> serde_json::Value {
    serde_json::json!({
        "type": "notifications_read_all",
        "data": { "unread_count": unread_count }
    })
}

fn notifications_cleared_event() -> serde_json::Value {
    serde_json::json!({ "type": "notifications_cleared" })
}
```

- [ ] **Step 4: Wire the three handlers to broadcast**

Replace `api_mark_notification_read` (lines ~101-112) with:

```rust
/// PATCH /api/notifications/{id}  — mark single notification read
pub(crate) async fn api_mark_notification_read(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match crate::db::notifications::mark_read(&infra.db, id).await {
        Ok(updated) => {
            if updated {
                let unread = crate::db::notifications::count_unread(&infra.db)
                    .await
                    .unwrap_or(0);
                bus.ui_event_tx
                    .send(notification_read_event(id, unread).to_string())
                    .ok();
            }
            Json(serde_json::json!({"ok": true, "updated": updated})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
```

Replace `api_mark_all_notifications_read` (lines ~115-125) with:

```rust
pub(crate) async fn api_mark_all_notifications_read(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    match crate::db::notifications::mark_all_read(&infra.db).await {
        Ok(count) => {
            // After mark-all, unread count is authoritatively 0.
            bus.ui_event_tx
                .send(notifications_read_all_event(0).to_string())
                .ok();
            Json(serde_json::json!({"ok": true, "updated": count})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
```

Replace `api_clear_all_notifications` (lines ~127-140) with:

```rust
pub(crate) async fn api_clear_all_notifications(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    match sqlx::query("DELETE FROM notifications")
        .execute(&infra.db)
        .await
    {
        Ok(r) => {
            bus.ui_event_tx
                .send(notifications_cleared_event().to_string())
                .ok();
            Json(serde_json::json!({"ok": true, "deleted": r.rows_affected()})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
```

- [ ] **Step 5: Verify it compiles**

Run: `make check`
Expected: no errors. (`ChannelBus`, `InfraServices`, `Uuid`, `State`, `Path` are all already imported in this file; `AppState: FromRef<ChannelBus>` already holds — the POST handler already takes `State(bus)`.)
Then (server): `make test-db` → the three `*_event_shape` tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/notifications.rs
git commit -m "feat(notifications): broadcast read/read-all/cleared events for cross-tab sync"
```

---

## Task 3: Frontend — notification store (seq counter, apply* actions, markRead fix)

**Files:**
- Modify: `ui/src/stores/notification-store.ts` (full rewrite of the store body)
- Test: `ui/src/stores/notification-store.test.ts` (**create**)

**Interfaces:**
- Produces (consumed by Tasks 4-6):
  - `newArrivalSeq: number` — monotonically bumped only on a genuine (non-duplicate) `prependNotification`.
  - `applyRead(id: string, unread_count: number): void`
  - `applyReadAll(unread_count: number): void`
  - `applyCleared(): void`
  - `resolveApproval(approvalId: string): void` — marks the unread `tool_approval` row whose `data.approval_id === approvalId` as read and decrements.

- [ ] **Step 1: Write the failing test**

Create `ui/src/stores/notification-store.test.ts`:

```ts
import { describe, it, expect, beforeEach } from "vitest";
import { useNotificationStore } from "./notification-store";
import type { NotificationRow } from "@/types/api";

function row(id: string, extra: Partial<NotificationRow> = {}): NotificationRow {
  return {
    id,
    type: "agent_error",
    title: "t",
    body: "b",
    data: {},
    read: false,
    created_at: "2026-07-14T00:00:00Z",
    ...extra,
  };
}

beforeEach(() => {
  useNotificationStore.setState({
    notifications: [],
    unread_count: 0,
    newArrivalSeq: 0,
  });
});

describe("notification-store", () => {
  it("prepend bumps unread_count and newArrivalSeq", () => {
    useNotificationStore.getState().prependNotification(row("a"));
    const s = useNotificationStore.getState();
    expect(s.unread_count).toBe(1);
    expect(s.newArrivalSeq).toBe(1);
    expect(s.notifications).toHaveLength(1);
  });

  it("duplicate prepend does not bump seq or count", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("a"));
    const s = useNotificationStore.getState();
    expect(s.unread_count).toBe(1);
    expect(s.newArrivalSeq).toBe(1);
    expect(s.notifications).toHaveLength(1);
  });

  it("markRead decrements once for an unread row", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.markRead("a");
    expect(useNotificationStore.getState().unread_count).toBe(0);
  });

  it("markRead does NOT decrement an already-read row", () => {
    useNotificationStore.setState({
      notifications: [row("a", { read: true })],
      unread_count: 0,
      newArrivalSeq: 0,
    });
    useNotificationStore.getState().markRead("a");
    expect(useNotificationStore.getState().unread_count).toBe(0);
  });

  it("applyRead sets read + server unread_count", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("b"));
    st.applyRead("a", 1);
    const s = useNotificationStore.getState();
    expect(s.notifications.find((n) => n.id === "a")?.read).toBe(true);
    expect(s.unread_count).toBe(1);
  });

  it("applyReadAll marks all read + sets count", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("b"));
    st.applyReadAll(0);
    const s = useNotificationStore.getState();
    expect(s.notifications.every((n) => n.read)).toBe(true);
    expect(s.unread_count).toBe(0);
  });

  it("applyCleared empties the list", () => {
    useNotificationStore.getState().prependNotification(row("a"));
    useNotificationStore.getState().applyCleared();
    const s = useNotificationStore.getState();
    expect(s.notifications).toHaveLength(0);
    expect(s.unread_count).toBe(0);
  });

  it("resolveApproval marks the matching unread approval row read", () => {
    useNotificationStore.setState({
      notifications: [
        row("n1", { type: "tool_approval", data: { approval_id: "ap-1" } }),
      ],
      unread_count: 1,
      newArrivalSeq: 0,
    });
    useNotificationStore.getState().resolveApproval("ap-1");
    const s = useNotificationStore.getState();
    expect(s.notifications[0].read).toBe(true);
    expect(s.unread_count).toBe(0);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ui && npm test -- notification-store`
Expected: FAIL — `newArrivalSeq` undefined / `applyRead is not a function` / `resolveApproval is not a function`.

- [ ] **Step 3: Write minimal implementation**

Replace the entire contents of `ui/src/stores/notification-store.ts` with:

```ts
import { create } from "zustand";
import { devtools } from "zustand/middleware";
import type { NotificationRow } from "@/types/api";

interface NotificationState {
  notifications: NotificationRow[];
  unread_count: number;
  /** Bumped only on a genuine (non-duplicate) live arrival. Drives sound/flash
   *  so refetch-on-reconnect and cold load never trigger the beep. */
  newArrivalSeq: number;
  setNotifications: (rows: NotificationRow[], count: number) => void;
  prependNotification: (row: NotificationRow) => void;
  markRead: (id: string) => void;
  markAllRead: () => void;
  clearAll: () => void;
  // Cross-tab / server-authoritative reconciliation (from WS events):
  applyRead: (id: string, unread_count: number) => void;
  applyReadAll: (unread_count: number) => void;
  applyCleared: () => void;
  resolveApproval: (approvalId: string) => void;
}

export const useNotificationStore = create<NotificationState>()(
  devtools(
    (set) => ({
      notifications: [],
      unread_count: 0,
      newArrivalSeq: 0,

      setNotifications: (rows, count) =>
        set({ notifications: rows, unread_count: count }, false, "setNotifications"),

      prependNotification: (row) =>
        set(
          (s) => {
            if (s.notifications.some((n) => n.id === row.id)) return s;
            return {
              notifications: [row, ...s.notifications],
              unread_count: s.unread_count + 1,
              newArrivalSeq: s.newArrivalSeq + 1,
            };
          },
          false,
          "prependNotification",
        ),

      markRead: (id) =>
        set(
          (s) => {
            const wasUnread = s.notifications.some((n) => n.id === id && !n.read);
            return {
              notifications: s.notifications.map((n) =>
                n.id === id ? { ...n, read: true } : n,
              ),
              unread_count: wasUnread
                ? Math.max(0, s.unread_count - 1)
                : s.unread_count,
            };
          },
          false,
          "markRead",
        ),

      markAllRead: () =>
        set(
          (s) => ({
            notifications: s.notifications.map((n) => ({ ...n, read: true })),
            unread_count: 0,
          }),
          false,
          "markAllRead",
        ),

      clearAll: () =>
        set({ notifications: [], unread_count: 0 }, false, "clearAll"),

      applyRead: (id, unread_count) =>
        set(
          (s) => ({
            notifications: s.notifications.map((n) =>
              n.id === id ? { ...n, read: true } : n,
            ),
            unread_count,
          }),
          false,
          "applyRead",
        ),

      applyReadAll: (unread_count) =>
        set(
          (s) => ({
            notifications: s.notifications.map((n) => ({ ...n, read: true })),
            unread_count,
          }),
          false,
          "applyReadAll",
        ),

      applyCleared: () =>
        set({ notifications: [], unread_count: 0 }, false, "applyCleared"),

      resolveApproval: (approvalId) =>
        set(
          (s) => {
            const target = s.notifications.find(
              (n) =>
                !n.read &&
                (n.data as Record<string, unknown>)?.approval_id === approvalId,
            );
            if (!target) return s;
            return {
              notifications: s.notifications.map((n) =>
                n.id === target.id ? { ...n, read: true } : n,
              ),
              unread_count: Math.max(0, s.unread_count - 1),
            };
          },
          false,
          "resolveApproval",
        ),
    }),
    { name: "NotificationStore", enabled: process.env.NODE_ENV !== "production" },
  ),
);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd ui && npm test -- notification-store`
Expected: all 8 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/stores/notification-store.ts ui/src/stores/notification-store.test.ts
git commit -m "feat(notifications): store seq counter, apply* reconcilers, markRead fix"
```

---

## Task 4: Frontend — WS event types, sync handlers, recovery refetch

**Files:**
- Modify: `ui/src/types/ws.ts` (add 3 interfaces + union members, ~lines 64-97)
- Modify: `ui/src/lib/queries.ts` (`useNotifications` ~678, `useNotificationWsSync` ~738; add `useNotificationRecovery`)

**Interfaces:**
- Consumes: store actions from Task 3 (`applyRead`, `applyReadAll`, `applyCleared`); backend events from Task 2; `useWsStore` `connected` flag; `qk.notifications`.
- Produces: `useNotificationRecovery()` hook (consumed by Task 5's bell mount).

- [ ] **Step 1: Add the WS event type interfaces**

In `ui/src/types/ws.ts`, add these interfaces next to `WsNotification` (~line 66):

```ts
export interface WsNotificationRead {
  type: "notification_read";
  data: { id: string; unread_count: number };
}

export interface WsNotificationsReadAll {
  type: "notifications_read_all";
  data: { unread_count: number };
}

export interface WsNotificationsCleared {
  type: "notifications_cleared";
}
```

Then add them to the `WsEvent` union (~lines 86-97):

```ts
export type WsEvent =
  | WsSessionUpdated
  | WsAgentProcessing
  | WsApprovalRequested
  | WsLog
  | WsCanvasUpdate
  | WsChannelsChanged
  | WsApprovalResolved
  | WsAuditEvent
  | WsNotification
  | WsNotificationRead
  | WsNotificationsReadAll
  | WsNotificationsCleared
  | WsPong
  | WsFileJobProgress;
```

- [ ] **Step 2: Verify types compile**

Run: `cd ui && npx tsc --noEmit`
Expected: no new errors (the new members are unused so far — that's fine).

- [ ] **Step 3: Extend the WS sync hook + add recovery**

In `ui/src/lib/queries.ts`, ensure the imports include `useRef` from `react`, `useQueryClient` from `@tanstack/react-query`, and `useWsStore` from `@/stores/ws-store` (add any that are missing — `useEffect` and `useQueryClient` are already used elsewhere in this file).

Replace `useNotificationWsSync` (~line 738) with:

```ts
export function useNotificationWsSync() {
  const prependNotification = useNotificationStore((s) => s.prependNotification);
  const applyRead = useNotificationStore((s) => s.applyRead);
  const applyReadAll = useNotificationStore((s) => s.applyReadAll);
  const applyCleared = useNotificationStore((s) => s.applyCleared);
  const resolveApproval = useNotificationStore((s) => s.resolveApproval);

  useWsSubscription("notification", (event) => {
    prependNotification(event.data);
  });
  useWsSubscription("notification_read", (event) => {
    applyRead(event.data.id, event.data.unread_count);
  });
  useWsSubscription("notifications_read_all", (event) => {
    applyReadAll(event.data.unread_count);
  });
  useWsSubscription("notifications_cleared", () => {
    applyCleared();
  });
  // N7: when an approval is resolved anywhere (toast / channel / another tab),
  // mark its persistent bell row read so it stops lingering unread.
  useWsSubscription("approval_resolved", (event) => {
    resolveApproval(event.approval_id);
  });
}

/**
 * N1 recovery: when the WS transitions disconnected -> connected, any
 * notifications created during the outage exist only in the DB. Refetch the
 * (newest-first, capped) list to reconcile the badge and recent items.
 */
export function useNotificationRecovery() {
  const qc = useQueryClient();
  const connected = useWsStore((s) => s.connected);
  const prev = useRef(connected);
  useEffect(() => {
    if (connected && !prev.current) {
      qc.invalidateQueries({ queryKey: qk.notifications });
    }
    prev.current = connected;
  }, [connected, qc]);
}
```

- [ ] **Step 4: Add focus + interval safety-net refetch to `useNotifications`**

Replace the `useQuery` options object inside `useNotifications` (~line 680) so it reads:

```ts
  const query = useQuery({
    queryKey: qk.notifications,
    queryFn: () => apiGet<NotificationsResponse>("/api/notifications?limit=20&offset=0"),
    refetchOnWindowFocus: true,
    refetchInterval: 60_000,
    refetchIntervalInBackground: false,
  });
```

(This covers a silent broadcast `Lagged` gap and a backgrounded-then-focused tab; the reconnect invalidate in Step 3 covers a dead-socket ping-timeout faster than the 60s poll.)

- [ ] **Step 5: Verify it compiles**

Run: `cd ui && npx tsc --noEmit`
Expected: no errors. Then `cd ui && npm run build` to confirm the production build is clean.

- [ ] **Step 6: Commit**

```bash
git add ui/src/types/ws.ts ui/src/lib/queries.ts
git commit -m "feat(notifications): WS read-sync handlers + reconnect/focus recovery refetch"
```

---

## Task 5: Frontend — sound/flash from `newArrivalSeq` + mount recovery

**Files:**
- Modify: `ui/src/components/notification-bell.tsx` (the sound/flash effect ~lines 116-143)

**Interfaces:**
- Consumes: `newArrivalSeq` (Task 3), `useNotificationRecovery` (Task 4).

- [ ] **Step 1: Replace the unread-delta sound effect with a seq-driven one**

In `ui/src/components/notification-bell.tsx`, inside `NotificationBell()`:

Add the recovery import to the existing `@/lib/queries` import line so it includes `useNotificationRecovery`, e.g.:

```ts
import {
  useNotifications,
  useNotificationWsSync,
  useNotificationRecovery,
  useMarkNotificationRead,
  useMarkAllRead,
  useClearAllNotifications,
} from "@/lib/queries";
```

Replace the `flashing` / `prevUnreadRef` declarations and the sound effect (~lines 122-143) with:

```ts
  const newArrivalSeq = useNotificationStore((s) => s.newArrivalSeq);

  const [flashing, setFlashing] = useState(false);
  const prevSeqRef = useRef(newArrivalSeq);

  // Fetch initial notifications, wire WS real-time updates + reconnect recovery
  useNotifications();
  useNotificationWsSync();
  useNotificationRecovery();

  const markRead = useMarkNotificationRead();
  const markAllRead = useMarkAllRead();
  const clearAll = useClearAllNotifications();

  // Flash + sound ONLY on a genuine live arrival (newArrivalSeq bump).
  // Refetch-on-reconnect and the initial cold-load fetch do not bump the seq,
  // so they never beep.
  useEffect(() => {
    if (newArrivalSeq > prevSeqRef.current) {
      setFlashing(true);
      playNotificationSound();
      const timer = setTimeout(() => setFlashing(false), 1500);
      prevSeqRef.current = newArrivalSeq;
      return () => clearTimeout(timer);
    }
    prevSeqRef.current = newArrivalSeq;
  }, [newArrivalSeq]);
```

Remove the now-dead `unread_count`-delta effect and the `prevUnreadRef` line if any remain. Keep the `unread_count` selector only if it is still used to render the badge number (it is — leave `const unread_count = useNotificationStore((s) => s.unread_count);` in place).

- [ ] **Step 2: Verify it compiles**

Run: `cd ui && npx tsc --noEmit`
Expected: no errors. Confirm no unused-variable lint error for a leftover `prevUnreadRef`.

- [ ] **Step 3: Manual verification (sound gating)**

Run: `cd ui && npm run dev`, log in, then:
1. **Cold load:** hard-refresh the page with existing unread notifications → **no beep** (was a bug before).
2. **Live arrival:** trigger a notification (e.g. cause an `agent_error`, or `POST /api/notifications` with a Bearer token) → **one beep + flash**.
3. **Reconnect:** kill the network briefly (DevTools offline → online) so the WS reconnects → badge/count refresh, **no beep**.

- [ ] **Step 4: Commit**

```bash
git add ui/src/components/notification-bell.tsx
git commit -m "feat(notifications): sound/flash on genuine arrivals only; mount reconnect recovery"
```

---

## Task 6: Integration verification (cross-tab + recovery E2E)

**Files:** none (verification only).

This task confirms the whole phase works end-to-end. No code; run against a dev build.

- [ ] **Step 1: Build backend + start**

Run: `make check` (must pass), then run the core locally or deploy to the dev server per project convention (`make remote-deploy` if testing on the server).

- [ ] **Step 2: Cross-tab read-state sync (N2)**

Open the UI in **two** browser tabs, both logged in.
1. Ensure both show the same unread badge count.
2. In tab A, open the bell and click a notification (marks it read) → tab A badge decrements.
3. Within ~1s, **tab B's badge decrements too** (via the `notification_read` WS event) — previously it stayed stale.
4. In tab A, "mark all read" → **both** tabs go to 0.
5. In tab A, "clear all" → **both** tabs empty the list.

- [ ] **Step 3: Missed-event recovery (N1)**

1. In tab A, open DevTools → Network → set **Offline**.
2. While offline, trigger a notification server-side (e.g. `POST /api/notifications` with a Bearer token, or cause an `agent_error`).
3. Set tab A back **Online** → within a moment the WS reconnects and the badge/list refetch shows the notification that arrived during the outage. Confirm **no beep** on this recovery refetch.

- [ ] **Step 4: Approval linkage (N7)**

1. Cause an agent to request a tool approval (produces both the 30s toast and a persistent `tool_approval` bell row) → confirm **exactly one** beep.
2. Approve/reject via the toast → the `approval_resolved` event marks the bell row read (its unread styling clears). Confirm badge decrements once.

- [ ] **Step 5: Record results**

Note pass/fail for each check above in the PR/commit description. If any check fails, debug before considering Phase 1 complete.

---

## Self-Review

**Spec coverage (Phase 1 items from `2026-07-14-chat-notifications-robustness-design.md` §4):**
- **N1 (recovery)** → Task 4 (reconnect invalidate + focus/interval refetch) + Task 6 Step 3. *Deviation, noted in Global Constraints:* Phase 1 uses refetch of the capped list instead of a `?since` cursor endpoint; the cursor endpoint is deferred to Phase 2 where pagination needs it. Rationale: for a 20-row capped list with a global unread COUNT, refetch is fully correct and much smaller.
- **N2 (cross-tab read)** → Task 1 (`count_unread`), Task 2 (broadcast events), Task 4 (handlers), Task 6 Step 2.
- **N5 (unread drift fix)** → Task 3 (`markRead` no-over-decrement + `apply*` set count from server truth).
- **N4 (sound on load)** → Task 3 (`newArrivalSeq`) + Task 5 (seq-driven effect) + Task 6 Step 3.
- **N7 (toast↔bell dedup)** → single beep guaranteed by Task 5; resolve-linkage via Task 4 `approval_resolved` handler + Task 3 `resolveApproval`; verified Task 6 Step 4.

**Placeholder scan:** none — every step has concrete code/commands.

**Type consistency:** `newArrivalSeq`, `applyRead(id, unread_count)`, `applyReadAll(unread_count)`, `applyCleared()`, `resolveApproval(approvalId)` are defined in Task 3 and consumed with identical signatures in Tasks 4-5. Backend event shapes in Task 2 (`{data:{id,unread_count}}`, `{data:{unread_count}}`, bare) match the `WsNotificationRead`/`WsNotificationsReadAll`/`WsNotificationsCleared` interfaces in Task 4. `event.approval_id` (top-level, per existing `WsApprovalResolved`) is used correctly in Task 4.

**Out-of-scope note (not fixed here):** the `approval_requested` toast in `layout.tsx` destructures `tool: toolName` but the backend broadcasts `tool_name` (approval_manager.rs:119) — a likely pre-existing display bug ("Agent: undefined"). Left untouched to keep Phase 1 focused; flag for a separate fix.
