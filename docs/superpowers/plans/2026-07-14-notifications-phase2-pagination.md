# Notifications Phase 2 — Pagination & History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the notification bell scroll back through the full history (beyond the newest 20) via cursor pagination, without losing loaded pages when the Phase 1 periodic/focus/reconnect refetch fires.

**Architecture:** Additive, no schema change. Backend gains a cursor query `list_notifications_before(created_at, id, limit)` and two optional query params (`before`, `before_id`) on `GET /api/notifications`. Frontend keeps the **zustand store as the list source-of-truth** (reusing every Phase 1 reconciler unchanged) and adds two actions: `appendOlder` (history pages) and `syncFirstPage` (a merge that replaces the old replace-on-refetch `setNotifications`, so loaded history survives a refetch). The bell loads older pages on scroll-to-bottom.

**Tech Stack:** Rust/Axum + sqlx (backend), TypeScript/React + Zustand + React Query + vitest (frontend).

## Global Constraints

- **No DB migration in Phase 2** — reuses the existing `notifications` table; only a new SELECT and two query params.
- **`notifications.id` is a UUID (not monotonic)** — the pagination cursor is the composite `(created_at, id)`. "Older than" = Postgres row comparison `(created_at, id) < ($cursor_ts, $cursor_id)`, ordered `created_at DESC, id DESC`.
- **`#[sqlx::test]` MUST carry `migrations = "../../migrations"`** — a bare `#[sqlx::test]` creates an empty ephemeral DB and every query fails with `relation "notifications" does not exist` (this exact bug shipped in Phase 1 and was fixed post-hoc). Every sqlx test in this plan uses the attribute.
- **Windows cannot run the Rust test binaries (they crash)** — verify Rust with `make check`; run the sqlx tests via the server (`postgres-test` on `127.0.0.1:5434`, `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test`, throttled `CARGO_BUILD_JOBS=4 nice -n 19 ionice -c3 cargo test -p opex-db notifications::tests`). Frontend vitest runs locally from `ui/`.
- **No gen-types drift** — the response stays `NotificationsResponseDto { items, unread_count, limit, offset }`; no Rust `ts_rs` DTO change, so no `api.generated.ts` regeneration. The new WS handling reuses Phase 1 store actions.
- **Commit to `master`; do NOT add any `Co-Authored-By` / Claude attribution.** Frontend from `ui/`.

### Design deviation from spec (documented, like Phase 1's N1)

The spec (§5) proposed migrating the list into a React Query `useInfiniteQuery` cache and shrinking the store. **This plan instead keeps the zustand store as the list SoT** and adds cursor pagination on top. Rationale: it reuses every Phase 1 reconciler (`prependNotification`, `applyRead`, `applyReadAll`, `applyCleared`, `resolveApproval`, `newArrivalSeq`) **unchanged and already deployed**, avoids re-implementing live-event handling against paginated cache pages, and the `syncFirstPage` merge keeps loaded history intact across the Phase 1 refetch. The spec's infinite-cache migration remains a valid future refactor if dual-state ever bites.

---

## File Structure

- `crates/opex-db/src/notifications.rs` — add `list_notifications_before(...)` + its sqlx test.
- `crates/opex-core/src/gateway/handlers/notifications.rs` — extend `ListQuery` with `before`/`before_id`; branch `api_list_notifications` into cursor mode.
- `ui/src/stores/notification-store.ts` — add `appendOlder`; replace `setNotifications` with `syncFirstPage` (merge).
- `ui/src/stores/notification-store.test.ts` — add tests for both.
- `ui/src/lib/queries.ts` — point `useNotifications` at `syncFirstPage`; add `useLoadOlderNotifications`.
- `ui/src/components/notification-bell.tsx` — scroll-to-load-older + loading footer.

---

## Task 1: Backend — `list_notifications_before` cursor query

**Files:**
- Modify: `crates/opex-db/src/notifications.rs` (add fn after `count_unread`; add test to the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub async fn list_notifications_before(db: &PgPool, before: chrono::DateTime<chrono::Utc>, before_id: Uuid, limit: i64) -> anyhow::Result<(Vec<Notification>, i64)>` — rows strictly older than the `(before, before_id)` cursor, newest-first, plus total unread count. Consumed by Task 2.

- [ ] **Step 1: Write the failing test**

Add inside the existing `#[cfg(test)] mod tests` block in `crates/opex-db/src/notifications.rs`:

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn list_notifications_before_paginates_older(pool: PgPool) -> Result<()> {
        create_notification(&pool, "agent_error", "a", "", serde_json::json!({})).await?;
        create_notification(&pool, "agent_error", "b", "", serde_json::json!({})).await?;
        create_notification(&pool, "agent_error", "c", "", serde_json::json!({})).await?;

        // Full newest-first list (a<b<c by created_at, so order is [c, b, a]).
        let (all, _) = list_notifications(&pool, 10, 0).await?;
        assert_eq!(all.len(), 3);

        // Cursor = newest row → expect the rest in the SAME order the full list has.
        let (older, unread) =
            list_notifications_before(&pool, all[0].created_at, all[0].id, 10).await?;
        assert_eq!(
            older.iter().map(|n| n.id).collect::<Vec<_>>(),
            all[1..].iter().map(|n| n.id).collect::<Vec<_>>(),
        );
        assert_eq!(unread, 3);

        // limit is honored: only the first older row.
        let (one, _) = list_notifications_before(&pool, all[0].created_at, all[0].id, 1).await?;
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, all[1].id);
        Ok(())
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo check -p opex-db` (Windows) — expect **compile error**: `cannot find function 'list_notifications_before'`.
(Execution deferred to the server per Global Constraints.)

- [ ] **Step 3: Write minimal implementation**

Insert after `count_unread` in `crates/opex-db/src/notifications.rs`:

```rust
/// List notifications strictly older than the `(created_at, id)` cursor,
/// newest-first. Returns (rows, `total_unread_count`). Powers history
/// pagination in the notification bell. Cursor is composite because `id` is a
/// UUID (not monotonic) — ties on `created_at` are broken by `id`.
pub async fn list_notifications_before(
    db: &PgPool,
    before: chrono::DateTime<chrono::Utc>,
    before_id: Uuid,
    limit: i64,
) -> Result<(Vec<Notification>, i64)> {
    let rows = sqlx::query_as::<_, Notification>(
        r"
        SELECT id, type AS notification_type, title, body, data, read, created_at
        FROM notifications
        WHERE (created_at, id) < ($1, $2)
        ORDER BY created_at DESC, id DESC
        LIMIT $3
        ",
    )
    .bind(before)
    .bind(before_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let unread: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read = FALSE")
        .fetch_one(db)
        .await?;

    Ok((rows, unread))
}
```

- [ ] **Step 4: Verify it compiles**

Run: `make check`
Expected: clean. Then (server): the throttled `cargo test -p opex-db notifications::tests` → `list_notifications_before_paginates_older` PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-db/src/notifications.rs
git commit -m "feat(notifications): list_notifications_before cursor query for history pagination"
```

---

## Task 2: Backend — cursor query params on `GET /api/notifications`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/notifications.rs` (`ListQuery` struct ~lines 67-75; `api_list_notifications` ~lines 79-98)

**Interfaces:**
- Consumes: `list_notifications_before` (Task 1).
- Produces: `GET /api/notifications?before=<RFC3339>&before_id=<uuid>&limit=<n>` returns notifications older than the cursor (same `NotificationsResponseDto`). Without both params, behaviour is unchanged (offset paging). Consumed by Task 4.

- [ ] **Step 1: Extend `ListQuery`**

Replace the existing `ListQuery` struct with (keep whatever default mechanism the file already uses for `limit`/`offset`; only the two `Option` fields are new):

```rust
#[derive(serde::Deserialize)]
pub(crate) struct ListQuery {
    #[serde(default = "default_list_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    /// History cursor: RFC3339 `created_at` of the oldest row already loaded.
    #[serde(default)]
    before: Option<String>,
    /// History cursor tiebreak: `id` of that same oldest row.
    #[serde(default)]
    before_id: Option<Uuid>,
}

fn default_list_limit() -> i64 {
    50
}
```

If the file already defines a `default_list_limit` (or an inline `#[serde(default)]` that yields 50), reuse it and do NOT duplicate — adapt this struct to the existing default. State in your report which default you used.

- [ ] **Step 2: Branch the handler into cursor mode**

Replace `api_list_notifications` with:

```rust
/// GET /api/notifications?limit=50&offset=0
/// GET /api/notifications?limit=20&before=<rfc3339>&before_id=<uuid>  (history page)
pub(crate) async fn api_list_notifications(
    State(infra): State<InfraServices>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.clamp(1, 200);
    let offset = q.offset.max(0);

    let result = match (q.before.as_deref(), q.before_id) {
        (Some(before_str), Some(before_id)) => {
            match chrono::DateTime::parse_from_rfc3339(before_str) {
                Ok(dt) => {
                    crate::db::notifications::list_notifications_before(
                        &infra.db,
                        dt.with_timezone(&chrono::Utc),
                        before_id,
                        limit,
                    )
                    .await
                }
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": format!("invalid `before` cursor: {e}")})),
                    )
                        .into_response();
                }
            }
        }
        _ => crate::db::notifications::list_notifications(&infra.db, limit, offset).await,
    };

    match result {
        Ok((items, unread_count)) => Json(crate::db::notifications::NotificationsResponseDto {
            items,
            unread_count,
            limit,
            offset,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `make check`
Expected: clean. (`Uuid`, `chrono`, `State`, `Query`, `Json`, `StatusCode` are already in scope in this file — confirm; add no duplicate imports.)

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/notifications.rs
git commit -m "feat(notifications): before/before_id cursor params on list endpoint"
```

---

## Task 3: Frontend store — `appendOlder` + `syncFirstPage` merge

**Files:**
- Modify: `ui/src/stores/notification-store.ts` (interface + actions; replace `setNotifications`)
- Modify: `ui/src/stores/notification-store.test.ts` (add tests)

**Interfaces:**
- Produces (consumed by Task 4):
  - `syncFirstPage(rows: NotificationRow[], unread_count: number): void` — merge the newest page into the list: refresh known rows (read-state), prepend genuinely-new rows, adopt server `unread_count`, **do not** bump `newArrivalSeq`, **preserve** older loaded rows. Replaces `setNotifications`.
  - `appendOlder(rows: NotificationRow[]): void` — dedup-append an older page to the tail; no `unread_count`/`newArrivalSeq` change.

- [ ] **Step 1: Write the failing tests**

Add to `ui/src/stores/notification-store.test.ts` (reuse the existing `row()` helper and `beforeEach` reset; extend the reset to include any new fields if needed):

```ts
  it("syncFirstPage merges: refreshes known, prepends new, preserves older, sets count, no beep", () => {
    // Seed: 3 rows loaded (b newest ... but list order is [new..old]); pretend
    // "old1"/"old2" are older pages already appended.
    useNotificationStore.setState({
      notifications: [row("n2", { read: false }), row("old1"), row("old2")],
      unread_count: 3,
      newArrivalSeq: 5,
    });
    // Server first page: a brand-new "n3", n2 now read, (old rows not in page).
    useNotificationStore
      .getState()
      .syncFirstPage([row("n3"), row("n2", { read: true })], 1);
    const s = useNotificationStore.getState();
    expect(s.notifications.map((n) => n.id)).toEqual(["n3", "n2", "old1", "old2"]);
    expect(s.notifications.find((n) => n.id === "n2")?.read).toBe(true); // refreshed
    expect(s.unread_count).toBe(1); // server value
    expect(s.newArrivalSeq).toBe(5); // unchanged — refetch never beeps
  });

  it("appendOlder dedup-appends to the tail without touching count or seq", () => {
    useNotificationStore.setState({
      notifications: [row("a"), row("b")],
      unread_count: 2,
      newArrivalSeq: 4,
    });
    // "b" is a duplicate (boundary overlap); "c","d" are genuinely older.
    useNotificationStore.getState().appendOlder([row("b"), row("c"), row("d")]);
    const s = useNotificationStore.getState();
    expect(s.notifications.map((n) => n.id)).toEqual(["a", "b", "c", "d"]);
    expect(s.unread_count).toBe(2);
    expect(s.newArrivalSeq).toBe(4);
  });
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ui && npm test -- notification-store`
Expected: FAIL — `syncFirstPage is not a function` / `appendOlder is not a function`.

- [ ] **Step 3: Implement**

In `ui/src/stores/notification-store.ts`:

Change the interface: remove `setNotifications` and add the two new signatures:

```ts
  syncFirstPage: (rows: NotificationRow[], unread_count: number) => void;
  appendOlder: (rows: NotificationRow[]) => void;
```

Replace the `setNotifications` action body with these two actions:

```ts
      // First-page (newest) refetch — MERGE, not replace, so history pages
      // loaded via appendOlder survive the Phase 1 periodic/focus/reconnect
      // refetch. Refreshes read-state of known rows, prepends genuinely-new
      // rows (server order = newest-first), adopts the server unread_count, and
      // never bumps newArrivalSeq (refetch must stay silent — only live WS
      // arrivals beep).
      syncFirstPage: (rows, unread_count) =>
        set(
          (s) => {
            const existing = new Set(s.notifications.map((n) => n.id));
            const fresh = rows.filter((r) => !existing.has(r.id));
            const byId = new Map(rows.map((r) => [r.id, r]));
            const merged = s.notifications.map((n) => byId.get(n.id) ?? n);
            return { notifications: [...fresh, ...merged], unread_count };
          },
          false,
          "syncFirstPage",
        ),

      // Append an older history page. Dedup by id (boundary rows can overlap
      // the live head), preserve order. Never touches unread_count/newArrivalSeq.
      appendOlder: (rows) =>
        set(
          (s) => {
            const existing = new Set(s.notifications.map((n) => n.id));
            const older = rows.filter((r) => !existing.has(r.id));
            if (older.length === 0) return s;
            return { notifications: [...s.notifications, ...older] };
          },
          false,
          "appendOlder",
        ),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd ui && npm test -- notification-store`
Expected: all tests PASS (the 8 Phase 1 tests + 2 new). If any Phase 1 test referenced `setNotifications`, update it to `syncFirstPage` (same signature) — grep first: `grep -n setNotifications ui/src/stores/notification-store.test.ts`.

- [ ] **Step 5: Commit**

```bash
git add ui/src/stores/notification-store.ts ui/src/stores/notification-store.test.ts
git commit -m "feat(notifications): store appendOlder + syncFirstPage merge for history pagination"
```

---

## Task 4: Frontend — wire refetch to merge + add older-page loader

**Files:**
- Modify: `ui/src/lib/queries.ts` (`useNotifications` ~line 679; add `useLoadOlderNotifications`)

**Interfaces:**
- Consumes: `syncFirstPage`/`appendOlder` (Task 3); the `before`/`before_id` endpoint (Task 2).
- Produces: `useLoadOlderNotifications(): { loadOlder: () => Promise<void>, isLoading: boolean, hasMore: boolean }`. Consumed by Task 5.

- [ ] **Step 1: Point `useNotifications` at the merge action**

In `ui/src/lib/queries.ts`, change `useNotifications` to use `syncFirstPage` instead of `setNotifications`:

```ts
export function useNotifications() {
  const syncFirstPage = useNotificationStore((s) => s.syncFirstPage);
  const query = useQuery({
    queryKey: qk.notifications,
    queryFn: () => apiGet<NotificationsResponse>("/api/notifications?limit=20&offset=0"),
    refetchOnWindowFocus: true,
    refetchInterval: 60_000,
    refetchIntervalInBackground: false,
  });
  useEffect(() => {
    if (query.data) {
      syncFirstPage(query.data.items, query.data.unread_count);
    }
  }, [query.data, syncFirstPage]);
  return query;
}
```

- [ ] **Step 2: Add the older-page loader hook**

Ensure `useState` and `useCallback` are imported from `react` in this file (add whichever is missing to the existing `react` import). Then add after `useNotifications`:

```ts
/**
 * History pagination for the notification bell. Not a useQuery/useInfiniteQuery:
 * the live head of the list is owned by the store (WS prepends + first-page
 * merge), so we only ever fetch strictly-OLDER pages and append them. Uses the
 * `(created_at, id)` cursor of the oldest row currently in the store.
 */
export function useLoadOlderNotifications() {
  const appendOlder = useNotificationStore((s) => s.appendOlder);
  const [isLoading, setIsLoading] = useState(false);
  const [hasMore, setHasMore] = useState(true);

  const loadOlder = useCallback(async () => {
    if (isLoading || !hasMore) return;
    const list = useNotificationStore.getState().notifications;
    const oldest = list[list.length - 1];
    if (!oldest) return;
    setIsLoading(true);
    try {
      const page = await apiGet<NotificationsResponse>(
        `/api/notifications?limit=20&before=${encodeURIComponent(oldest.created_at)}&before_id=${oldest.id}`,
      );
      appendOlder(page.items);
      if (page.items.length < 20) setHasMore(false);
    } catch {
      // transient network error — allow retry on the next scroll
    } finally {
      setIsLoading(false);
    }
  }, [appendOlder, isLoading, hasMore]);

  return { loadOlder, isLoading, hasMore };
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd ui && npx tsc --noEmit` then `cd ui && npm run build`
Expected: clean. (Confirm no remaining reference to the removed `setNotifications` anywhere: `grep -rn setNotifications ui/src` should return nothing.)

- [ ] **Step 4: Commit**

```bash
git add ui/src/lib/queries.ts
git commit -m "feat(notifications): merge-on-refetch + useLoadOlderNotifications history hook"
```

---

## Task 5: Frontend — scroll-to-load-older in the bell

**Files:**
- Modify: `ui/src/components/notification-bell.tsx` (list scroll container ~line 202; imports)

**Interfaces:**
- Consumes: `useLoadOlderNotifications` (Task 4).

- [ ] **Step 1: Import the hook + a spinner icon**

Add `useLoadOlderNotifications` to the `@/lib/queries` import, and add `Loader2` to the existing `lucide-react` import:

```ts
import { Bell, Loader2 } from "lucide-react";
```

- [ ] **Step 2: Mount the hook and add a scroll handler**

Inside `NotificationBell()`, after the existing `useClearAllNotifications()` line, add:

```ts
  const { loadOlder, isLoading: loadingOlder, hasMore } = useLoadOlderNotifications();

  const onListScroll = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    // within 48px of the bottom → pull the next older page
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 48 && hasMore && !loadingOlder) {
      void loadOlder();
    }
  };
```

- [ ] **Step 3: Attach the handler + footer to the list container**

Add `onScroll={onListScroll}` to the scroll `div` (the one with `overflow-y-auto overscroll-contain`), and add a loading footer AFTER the `notifications.map(...)` block but still inside that scroll `div`:

```tsx
        <div
          className="max-h-[min(24rem,calc(100dvh-8rem))] overflow-y-auto overscroll-contain"
          onScroll={onListScroll}
        >
          {notifications.length === 0 ? (
            <div className="flex items-center justify-center py-8 text-sm text-muted-foreground">
              {t("notifications.empty")}
            </div>
          ) : (
            <>
              {notifications.map((n) => (
                /* …existing button unchanged… */
              ))}
              {loadingOlder && (
                <div className="flex items-center justify-center py-3">
                  <Loader2 size={16} className="animate-spin text-muted-foreground" />
                </div>
              )}
            </>
          )}
        </div>
```

Keep the existing notification `<button>` markup exactly as-is inside the `.map`. Only the wrapping `<>…</>`, the `onScroll`, and the spinner footer are new. (No i18n keys are added — the spinner has no text.)

- [ ] **Step 4: Verify it compiles + renders**

Run: `cd ui && npx tsc --noEmit` (clean, no unused `Loader2`/hook), then `cd ui && npm run build`.

- [ ] **Step 5: Manual check (dev)**

Run `cd ui && npm run dev`, log in, open the bell, and (with >20 notifications) scroll to the bottom of the dropdown → an older page loads and appends; a spinner shows briefly; scrolling past the end stops loading (no infinite requests). Trigger a live notification while scrolled → it prepends at top, loaded history stays.

- [ ] **Step 6: Commit**

```bash
git add ui/src/components/notification-bell.tsx
git commit -m "feat(notifications): scroll-to-load older history in the bell dropdown"
```

---

## Task 6: Verification (build + server sqlx + protocol)

**Files:** none.

- [ ] **Step 1: Full build**

Run: `make check` (Rust clean) and `cd ui && npm run build` (UI clean).

- [ ] **Step 2: Server sqlx run (deploy the branch first, then test)**

After the branch is pushed and the server has pulled it, on the server run the throttled scoped test:

```bash
ssh <server> 'cd ~/opex-src && . ~/.cargo/env && \
  DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  CARGO_BUILD_JOBS=4 nice -n 19 ionice -c3 cargo test -p opex-db notifications::tests -- --nocapture'
```

Expected: all `notifications::tests` PASS, including `list_notifications_before_paginates_older`.

- [ ] **Step 3: Protocol pagination check (optional, non-destructive)**

Against the deployed server (token stays server-side), verify the cursor endpoint: `GET /api/notifications?limit=1` → note the single item's `created_at`+`id` → `GET /api/notifications?limit=20&before=<created_at>&before_id=<id>` returns strictly-older rows (never the cursor row itself). A bad cursor (`before=not-a-date`) returns `400`.

- [ ] **Step 4: Manual browser check**

In the deployed UI, open the bell with >20 notifications and scroll to the bottom → older entries load; live arrivals still prepend; the badge/unread count stays correct.

---

## Self-Review

**Spec coverage (§5 N3):**
- Cursor endpoint (`before`/`before_id`) → Tasks 1-2.
- Infinite-scroll / load-more in the dropdown → Task 5.
- `unread_count` on every page response → preserved (both DB fns return it; DTO unchanged).
- **Deviation (documented in Global Constraints):** list stays in the zustand store (Tasks 3-4 add `appendOlder` + `syncFirstPage` merge) instead of migrating to a React Query infinite cache. Reuses all Phase 1 reconcilers; `syncFirstPage` prevents the periodic refetch from clobbering loaded history.

**Placeholder scan:** none — every step has concrete code/commands.

**Type consistency:** `list_notifications_before(db, before: DateTime<Utc>, before_id: Uuid, limit: i64) -> Result<(Vec<Notification>, i64)>` (Task 1) is called with exactly those types in Task 2. `syncFirstPage(rows, unread_count)` / `appendOlder(rows)` (Task 3) are consumed with identical signatures in Task 4. The endpoint shape `?before=&before_id=&limit=` (Task 2) matches the fetch URL in Task 4. Response stays `NotificationsResponseDto` — no ts-gen change.

**Interaction with Phase 1:** `syncFirstPage` deliberately does NOT bump `newArrivalSeq`, so the Phase 1 sound-gating (beep only on live WS arrival) is preserved. `appendOlder` never changes `unread_count`, so the badge stays server-authoritative. Removing `setNotifications` requires updating its sole caller (`useNotifications`, Task 4) and any test reference (Task 3 Step 4 greps).
