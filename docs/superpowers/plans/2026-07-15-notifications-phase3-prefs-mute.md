# Notifications Phase 3 — Preferences & Mute Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-type notification preferences (mute + sound), enforced globally: a **muted** type is still persisted (history/audit) but not broadcast live (no cross-tab badge bump, no sound, no toast); a **sound-off** type still arrives live but never beeps. Configured from a gear panel in the bell dropdown.

**Architecture:** New `notification_prefs(notification_type PK, muted, sound, updated_at)` table + a tiny opex-db query module. The single `notify()` chokepoint reads `is_muted(type)` and skips the WS broadcast when muted (still persists). `GET`/`PUT /api/notification-prefs` expose the rows. Frontend mirrors prefs into the zustand store; the WS `notification` handler passes a `silent` flag to `prependNotification` so sound-off types don't bump `newArrivalSeq` (the Phase 1 sound trigger); a gear in the bell header toggles a prefs panel of per-type mute/sound switches.

**Tech Stack:** Rust/Axum + sqlx (backend), TypeScript/React + Zustand + React Query + vitest (frontend).

## Global Constraints

- **This phase HAS a migration** — `085_notification_prefs.sql`. It runs automatically on startup (`sqlx::migrate` in `main.rs`) and `server-deploy.sh` syncs `migrations/` to the runtime dir. Use `CREATE TABLE IF NOT EXISTS`. **No CHECK constraint** on `notification_type` (the type set is open-ended — any string `notify()` is called with; a CHECK would need widening per new type, a known gotcha in m078/m082).
- **`notify()` is the single persist+broadcast chokepoint** (`crates/opex-core/src/gateway/handlers/notifications.rs`), reused by ~20 call sites across 17 distinct `notification_type` values. The mute check goes there once and every trigger inherits it.
- **`cargo check` / `make check` does NOT catch `clippy -D warnings`** — the deploy build and CI enforce `-D warnings`. After each backend task run `cargo clippy -p <crate> --all-targets -- -D warnings` (clippy runs fine on Windows; only Rust *test* binaries crash here).
- **Windows cannot run the Rust test binaries** — verify Rust with `cargo check` + `clippy`; run sqlx tests via the server (`postgres-test` on `127.0.0.1:5434`, `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test`, throttled `CARGO_BUILD_JOBS=4 nice -n 19 ionice -c3 cargo test -p opex-db notification_prefs::`). Every `#[sqlx::test]` MUST carry `migrations = "../../migrations"`. Frontend vitest runs locally from `ui/`.
- **No gen-types drift** — the prefs DTO is hand-typed on the frontend (a small `interface`); the Rust `NotificationPref` struct is plain serde (NO `ts_rs`/`ts-gen` derive), so `api.generated.ts` is not regenerated.
- **Commit to `master`; NO `Co-Authored-By`.** Frontend from `ui/`.

### Muted semantics (explicit)

A muted type is **persisted** by `create_notification` (so it stays in history and is counted in the DB `unread` total, surfacing on the next list refetch) but the live WS broadcast is skipped — so no live badge bump, no sound, no toast at arrival time. This is "mute the live noise, keep the record," matching the spec. It is NOT "never record it." Sound-off (not muted) types broadcast normally and are silenced client-side by type.

---

## File Structure

- `migrations/085_notification_prefs.sql` — **create** the table.
- `crates/opex-db/src/notification_prefs.rs` — **create** query module (`NotificationPref`, `list_prefs`, `is_muted`, `upsert_pref`) + sqlx test.
- `crates/opex-db/src/lib.rs` — register `pub mod notification_prefs;`.
- `crates/opex-core/src/gateway/handlers/notifications.rs` — `notify()` mute check; `GET`/`PUT /api/notification-prefs` handlers + route.
- `ui/src/stores/notification-store.ts` — add `prefs` + `setPrefs`; `prependNotification` gains an optional `silent` flag.
- `ui/src/stores/notification-store.test.ts` — tests for silent-prepend + setPrefs.
- `ui/src/lib/queries.ts` — `useNotificationPrefs` / `useUpdateNotificationPref`; sound-gate the WS `notification` handler.
- `ui/src/components/notification-bell.tsx` — gear toggle + prefs panel; mount `useNotificationPrefs`.
- `ui/src/i18n/locales/{en,ru}.json` — new `notifications.*` keys.

---

## Task 1: Backend — `notification_prefs` table + query module

**Files:**
- Create: `migrations/085_notification_prefs.sql`
- Create: `crates/opex-db/src/notification_prefs.rs`
- Modify: `crates/opex-db/src/lib.rs` (add `pub mod notification_prefs;` alongside the other `pub mod` lines)

**Interfaces:**
- Produces (consumed by Tasks 2-3):
  - `pub struct NotificationPref { notification_type: String (serde "type"), muted: bool, sound: bool }`
  - `pub async fn list_prefs(db: &PgPool) -> Result<Vec<NotificationPref>>`
  - `pub async fn is_muted(db: &PgPool, notification_type: &str) -> Result<bool>` (absent row → false)
  - `pub async fn upsert_pref(db: &PgPool, notification_type: &str, muted: bool, sound: bool) -> Result<()>`

- [ ] **Step 1: Write the migration**

Create `migrations/085_notification_prefs.sql`:

```sql
-- m085: notification_prefs — глобальные пер-типовые настройки уведомлений
-- (single-operator): mute + sound. Без CHECK на notification_type — набор типов
-- открытый (любая строка, с которой зовут notify()), CHECK пришлось бы расширять
-- на каждый новый тип.
CREATE TABLE IF NOT EXISTS notification_prefs (
    notification_type TEXT        PRIMARY KEY,
    muted             BOOLEAN     NOT NULL DEFAULT FALSE,
    sound             BOOLEAN     NOT NULL DEFAULT TRUE,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

- [ ] **Step 2: Write the failing test + module skeleton**

Create `crates/opex-db/src/notification_prefs.rs` with ONLY the test first (so it fails to compile until Step 3):

```rust
use anyhow::Result;
use sqlx::PgPool;

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn prefs_upsert_and_read(pool: PgPool) -> Result<()> {
        // Default: absent row → not muted.
        assert_eq!(is_muted(&pool, "agent_error").await?, false);

        upsert_pref(&pool, "agent_error", true, false).await?;
        assert_eq!(is_muted(&pool, "agent_error").await?, true);

        let all = list_prefs(&pool).await?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].notification_type, "agent_error");
        assert!(all[0].muted && !all[0].sound);

        // Upsert again flips the values (ON CONFLICT update).
        upsert_pref(&pool, "agent_error", false, true).await?;
        assert_eq!(is_muted(&pool, "agent_error").await?, false);
        let all2 = list_prefs(&pool).await?;
        assert_eq!(all2.len(), 1);
        assert!(!all2[0].muted && all2[0].sound);
        Ok(())
    }
}
```

Add `pub mod notification_prefs;` to `crates/opex-db/src/lib.rs` (next to the existing `pub mod notifications;` etc.).

- [ ] **Step 3: Run check to verify it fails**

Run: `cargo check -p opex-db` — expect **compile error**: `cannot find function 'is_muted'` / `list_prefs` / `upsert_pref`.

- [ ] **Step 4: Implement the module functions**

Add above the `#[cfg(test)]` block in `crates/opex-db/src/notification_prefs.rs`:

```rust
/// One per-type notification preference row (global; single-operator).
/// Absent row = defaults (muted=false, sound=true).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct NotificationPref {
    #[serde(rename = "type")]
    pub notification_type: String,
    pub muted: bool,
    pub sound: bool,
}

/// All configured preference rows, ordered by type. Types with no row use
/// the defaults (the caller/UI fills them in).
pub async fn list_prefs(db: &PgPool) -> Result<Vec<NotificationPref>> {
    let rows = sqlx::query_as::<_, NotificationPref>(
        "SELECT notification_type, muted, sound FROM notification_prefs ORDER BY notification_type",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Whether a given type is muted. Absent row → false (not muted).
pub async fn is_muted(db: &PgPool, notification_type: &str) -> Result<bool> {
    let muted: Option<bool> =
        sqlx::query_scalar("SELECT muted FROM notification_prefs WHERE notification_type = $1")
            .bind(notification_type)
            .fetch_optional(db)
            .await?;
    Ok(muted.unwrap_or(false))
}

/// Insert or update a preference row (UPSERT on `notification_type`).
pub async fn upsert_pref(
    db: &PgPool,
    notification_type: &str,
    muted: bool,
    sound: bool,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO notification_prefs (notification_type, muted, sound)
         VALUES ($1, $2, $3)
         ON CONFLICT (notification_type)
         DO UPDATE SET muted = EXCLUDED.muted, sound = EXCLUDED.sound, updated_at = now()",
    )
    .bind(notification_type)
    .bind(muted)
    .bind(sound)
    .execute(db)
    .await?;
    Ok(())
}
```

- [ ] **Step 5: Verify compile + clippy**

Run: `cargo check -p opex-db` (clean) then `cargo clippy -p opex-db --all-targets -- -D warnings` (clean).
Test execution deferred to the server (Step commands in Task 6).

- [ ] **Step 6: Commit**

```bash
git add migrations/085_notification_prefs.sql crates/opex-db/src/notification_prefs.rs crates/opex-db/src/lib.rs
git commit -m "feat(notifications): notification_prefs table + query module (mute/sound)"
```

---

## Task 2: Backend — GET/PUT `/api/notification-prefs`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/notifications.rs` (`routes()` ~lines 14-23; add two handlers + a body struct)

**Interfaces:**
- Consumes: `crate::db::notification_prefs::{list_prefs, upsert_pref}` (Task 1).
- Produces: `GET /api/notification-prefs` → `{ "prefs": [ {type, muted, sound}, … ] }`; `PUT /api/notification-prefs` body `{type, muted, sound}` → `{ "ok": true }` (400 on empty/oversized type). Consumed by Task 4.

- [ ] **Step 1: Add the route**

In `routes()`, add (after the existing notification routes, before the closing):

```rust
        .route(
            "/api/notification-prefs",
            get(api_get_notification_prefs).put(api_put_notification_prefs),
        )
```

- [ ] **Step 2: Add the handlers**

Add near the other handlers in `crates/opex-core/src/gateway/handlers/notifications.rs`:

```rust
/// GET /api/notification-prefs — all configured per-type prefs (absent = defaults).
pub(crate) async fn api_get_notification_prefs(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    match crate::db::notification_prefs::list_prefs(&infra.db).await {
        Ok(prefs) => Json(serde_json::json!({ "prefs": prefs })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct UpdatePrefBody {
    #[serde(rename = "type")]
    r#type: String,
    muted: bool,
    sound: bool,
}

/// PUT /api/notification-prefs — upsert one type's prefs.
pub(crate) async fn api_put_notification_prefs(
    State(infra): State<InfraServices>,
    Json(body): Json<UpdatePrefBody>,
) -> impl IntoResponse {
    if body.r#type.is_empty() || body.r#type.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid notification type"})),
        )
            .into_response();
    }
    match crate::db::notification_prefs::upsert_pref(&infra.db, &body.r#type, body.muted, body.sound)
        .await
    {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
```

(`get`, `put`, `State`, `InfraServices`, `Json`, `StatusCode`, `IntoResponse` are already imported in this file — confirm; add none twice.)

- [ ] **Step 3: Verify compile + clippy**

Run: `cargo check -p opex-core` (clean) then `cargo clippy -p opex-core --all-targets -- -D warnings` (clean).

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/notifications.rs
git commit -m "feat(notifications): GET/PUT /api/notification-prefs endpoints"
```

---

## Task 3: Backend — `notify()` mute enforcement

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/notifications.rs` (`notify()` ~lines 229-257)

**Interfaces:**
- Consumes: `crate::db::notification_prefs::is_muted` (Task 1).

- [ ] **Step 1: Add the mute check to `notify()`**

Replace the body of `notify()` (keep the signature identical) so the broadcast is gated:

```rust
pub async fn notify(
    db: &sqlx::PgPool,
    ui_event_tx: &tokio::sync::broadcast::Sender<String>,
    notification_type: &str,
    title: &str,
    body: &str,
    data: serde_json::Value,
) -> anyhow::Result<()> {
    let notification =
        crate::db::notifications::create_notification(db, notification_type, title, body, data)
            .await?;

    // Muted types are still PERSISTED (history/audit, counted in unread on the
    // next refetch) but not broadcast live — no cross-tab badge bump, no sound,
    // no toast at arrival. Fail-open: if the prefs read errors, broadcast anyway
    // (over-notifying beats silently dropping an alert).
    let muted = crate::db::notification_prefs::is_muted(db, notification_type)
        .await
        .unwrap_or(false);
    if !muted {
        ui_event_tx
            .send(serde_json::json!({"type": "notification", "data": notification}).to_string())
            .ok();
    }

    Ok(())
}
```

- [ ] **Step 2: Verify compile + clippy**

Run: `cargo check -p opex-core` (clean) then `cargo clippy -p opex-core --all-targets -- -D warnings` (clean).
(No unit test here — the broadcast-skip is integration behavior verified by the protocol E2E in Task 6; `is_muted` itself is covered by Task 1's sqlx test.)

- [ ] **Step 3: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/notifications.rs
git commit -m "feat(notifications): notify() skips live broadcast for muted types (still persists)"
```

---

## Task 4: Frontend — prefs hooks, store prefs, sound gating

**Files:**
- Modify: `ui/src/stores/notification-store.ts` (add `prefs` + `setPrefs`; `prependNotification` gains optional `silent`)
- Modify: `ui/src/stores/notification-store.test.ts` (add 2 tests)
- Modify: `ui/src/lib/queries.ts` (add `useNotificationPrefs` + `useUpdateNotificationPref`; sound-gate the WS `notification` handler)

**Interfaces:**
- Produces (consumed by Task 5): store `prefs: Record<string, {muted:boolean;sound:boolean}>`, `setPrefs(map)`; `useNotificationPrefs()`; `useUpdateNotificationPref()`; exported `NotificationPref` interface.

- [ ] **Step 1: Write the failing store tests**

Add to `ui/src/stores/notification-store.test.ts` (reuse `row()`/`beforeEach`; extend the `beforeEach` reset to include `prefs: {}` if the reset sets explicit state):

```ts
  it("prependNotification with silent=true adds + bumps unread but NOT newArrivalSeq", () => {
    useNotificationStore.setState({ notifications: [], unread_count: 0, newArrivalSeq: 3 });
    useNotificationStore.getState().prependNotification(row("a"), true);
    const s = useNotificationStore.getState();
    expect(s.notifications).toHaveLength(1);
    expect(s.unread_count).toBe(1);
    expect(s.newArrivalSeq).toBe(3); // silent → no sound trigger
  });

  it("setPrefs stores the prefs map", () => {
    useNotificationStore.getState().setPrefs({ agent_error: { muted: true, sound: false } });
    expect(useNotificationStore.getState().prefs.agent_error).toEqual({ muted: true, sound: false });
  });
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ui && npm test -- notification-store`
Expected: FAIL — `prependNotification` ignores the 2nd arg / `setPrefs is not a function` / `prefs` undefined.

- [ ] **Step 3: Implement the store changes**

In `ui/src/stores/notification-store.ts`:

Add to the `NotificationState` interface:

```ts
  prefs: Record<string, { muted: boolean; sound: boolean }>;
  setPrefs: (prefs: Record<string, { muted: boolean; sound: boolean }>) => void;
```

Change `prependNotification`'s signature in the interface to accept the flag:

```ts
  prependNotification: (row: NotificationRow, silent?: boolean) => void;
```

Add `prefs: {},` to the initial state (next to `newArrivalSeq: 0,`).

Replace the `prependNotification` action with the silent-aware version:

```ts
      prependNotification: (row, silent = false) =>
        set(
          (s) => {
            if (s.notifications.some((n) => n.id === row.id)) return s;
            return {
              notifications: [row, ...s.notifications],
              unread_count: s.unread_count + 1,
              // silent (sound-off pref) → do not bump the sound trigger, but the
              // row is still added and the badge still increments.
              newArrivalSeq: silent ? s.newArrivalSeq : s.newArrivalSeq + 1,
            };
          },
          false,
          "prependNotification",
        ),
```

Add the `setPrefs` action (next to the other actions):

```ts
      setPrefs: (prefs) => set({ prefs }, false, "setPrefs"),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd ui && npm test -- notification-store`
Expected: all pass (the 10 prior tests + 2 new).

- [ ] **Step 5: Add the prefs hooks + sound-gate the WS handler**

In `ui/src/lib/queries.ts`, add near the notification hooks:

```ts
export interface NotificationPref {
  type: string;
  muted: boolean;
  sound: boolean;
}
interface NotificationPrefsResponse {
  prefs: NotificationPref[];
}

export function useNotificationPrefs() {
  const setPrefs = useNotificationStore((s) => s.setPrefs);
  const query = useQuery({
    queryKey: ["notification-prefs"] as const,
    queryFn: () => apiGet<NotificationPrefsResponse>("/api/notification-prefs"),
  });
  useEffect(() => {
    if (query.data) {
      const map: Record<string, { muted: boolean; sound: boolean }> = {};
      for (const p of query.data.prefs) map[p.type] = { muted: p.muted, sound: p.sound };
      setPrefs(map);
    }
  }, [query.data, setPrefs]);
  return query;
}

export function useUpdateNotificationPref() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: NotificationPref) => apiPut("/api/notification-prefs", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["notification-prefs"] }),
    onError: (e: Error) => toast.error(e.message),
  });
}
```

(`toast` from `sonner` and `apiPut` are already imported in this file — confirm; add none twice.)

Then sound-gate the WS `notification` handler inside `useNotificationWsSync` — replace ONLY the `"notification"` subscription (leave the read/clear/approval_resolved ones unchanged):

```ts
  useWsSubscription("notification", (event) => {
    // Muted types never reach here (server skips the broadcast). Among the rest,
    // a sound-off pref means: add + bump badge, but don't trigger the beep.
    const pref = useNotificationStore.getState().prefs[event.data.type];
    prependNotification(event.data, pref?.sound === false);
  });
```

- [ ] **Step 6: Verify compile + build**

Run: `cd ui && npx tsc --noEmit` (clean) then `cd ui && npm run build`.

- [ ] **Step 7: Commit**

```bash
git add ui/src/stores/notification-store.ts ui/src/stores/notification-store.test.ts ui/src/lib/queries.ts
git commit -m "feat(notifications): prefs hooks + store prefs + WS sound gating by type"
```

---

## Task 5: Frontend — bell gear + prefs panel + i18n

**Files:**
- Modify: `ui/src/components/notification-bell.tsx` (header gear, prefs panel, mount `useNotificationPrefs`)
- Modify: `ui/src/i18n/locales/en.json` and `ui/src/i18n/locales/ru.json` (new keys)

**Interfaces:**
- Consumes: `useNotificationPrefs`, `useUpdateNotificationPref`, store `prefs` (Task 4); `Switch` from `@/components/ui/switch`.

- [ ] **Step 1: Add i18n keys**

Add these flat keys to `ui/src/i18n/locales/en.json` (next to the existing `notifications.*` block):

```json
  "notifications.settings": "Notification settings",
  "notifications.prefs_title": "Preferences",
  "notifications.back": "Back",
  "notifications.mute": "Mute",
  "notifications.sound": "Sound",
  "notifications.type.agent_error": "Agent errors",
  "notifications.type.tool_approval": "Tool approvals",
  "notifications.type.watchdog_alert": "Watchdog alerts",
  "notifications.type.access_request": "Access requests",
  "notifications.type.infra_decision": "Infra decisions",
  "notifications.type.initiative_proposal": "Initiative proposals",
```

Add the Russian equivalents to `ui/src/i18n/locales/ru.json`:

```json
  "notifications.settings": "Настройки уведомлений",
  "notifications.prefs_title": "Настройки",
  "notifications.back": "Назад",
  "notifications.mute": "Без уведомлений",
  "notifications.sound": "Звук",
  "notifications.type.agent_error": "Ошибки агента",
  "notifications.type.tool_approval": "Подтверждения инструментов",
  "notifications.type.watchdog_alert": "Оповещения watchdog",
  "notifications.type.access_request": "Запросы доступа",
  "notifications.type.infra_decision": "Инфра-решения",
  "notifications.type.initiative_proposal": "Инициативы",
```

- [ ] **Step 2: Add imports + the muteable-types constant**

In `ui/src/components/notification-bell.tsx`:
- Add `Settings` (gear) to the `lucide-react` import: `import { Bell, Loader2, Settings } from "lucide-react";`
- Add `useNotificationPrefs, useUpdateNotificationPref` to the `@/lib/queries` import.
- Add `import { Switch } from "@/components/ui/switch";`.
- Add a module-level constant (below the imports, near `ERROR_EVENTS`):

```ts
// The user-facing alerting types exposed in the prefs panel. The backend mute
// works for ANY type; this is the curated subset worth toggling.
const PREF_TYPES: { type: string; labelKey: string }[] = [
  { type: "agent_error", labelKey: "notifications.type.agent_error" },
  { type: "tool_approval", labelKey: "notifications.type.tool_approval" },
  { type: "watchdog_alert", labelKey: "notifications.type.watchdog_alert" },
  { type: "access_request", labelKey: "notifications.type.access_request" },
  { type: "infra_decision", labelKey: "notifications.type.infra_decision" },
  { type: "initiative_proposal", labelKey: "notifications.type.initiative_proposal" },
];
```

- [ ] **Step 3: Mount the prefs query + panel state**

Inside `NotificationBell()`, after the existing hooks, add:

```ts
  const prefs = useNotificationStore((s) => s.prefs);
  const [showPrefs, setShowPrefs] = useState(false);
  useNotificationPrefs();
  const updatePref = useUpdateNotificationPref();
```

- [ ] **Step 4: Add the gear to the header + render the panel conditionally**

In the header row (next to the title span at `~line 189`), add a gear button that toggles the panel:

```tsx
          <span className="text-sm font-semibold">{t("notifications.title")}</span>
          <button
            type="button"
            aria-label={t("notifications.settings")}
            className="ml-auto flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent"
            onClick={(e) => {
              e.preventDefault();
              setShowPrefs((v) => !v);
            }}
          >
            <Settings size={15} className={showPrefs ? "text-primary" : ""} />
          </button>
```

Then, replace the List block so that when `showPrefs` is true the panel renders instead of the notification list. The prefs panel:

```tsx
        {showPrefs ? (
          <div className="max-h-[min(24rem,calc(100dvh-8rem))] overflow-y-auto overscroll-contain p-2">
            {PREF_TYPES.map(({ type, labelKey }) => {
              const p = prefs[type] ?? { muted: false, sound: true };
              return (
                <div key={type} className="flex items-center gap-3 px-2 py-2">
                  <span className="flex-1 truncate text-sm">{t(labelKey)}</span>
                  <label className="flex items-center gap-1 text-2xs text-muted-foreground">
                    {t("notifications.mute")}
                    <Switch
                      size="sm"
                      checked={p.muted}
                      onCheckedChange={(muted) =>
                        updatePref.mutate({ type, muted, sound: p.sound })
                      }
                    />
                  </label>
                  <label className="flex items-center gap-1 text-2xs text-muted-foreground">
                    {t("notifications.sound")}
                    <Switch
                      size="sm"
                      checked={p.sound}
                      disabled={p.muted}
                      onCheckedChange={(sound) =>
                        updatePref.mutate({ type, muted: p.muted, sound })
                      }
                    />
                  </label>
                </div>
              );
            })}
          </div>
        ) : (
          /* …the existing List block (the <div onScroll…> …) unchanged… */
        )}
```

Keep the existing list `<div className="max-h-…" onScroll={onListScroll}> … </div>` EXACTLY as-is inside the `: (` branch. Only the `showPrefs ? (panel) : (list)` wrapper, the gear button, and the panel markup are new.

- [ ] **Step 5: Verify compile + build + lint**

Run: `cd ui && npx tsc --noEmit` (clean, no unused imports), `cd ui && npm run build`, `cd ui && npm run lint` (no NEW warnings from this file). Optionally confirm `en.json`/`ru.json` remain valid JSON (`node -e "require('./ui/src/i18n/locales/en.json')"`).

- [ ] **Step 6: Manual check (dev)**

Run `cd ui && npm run dev`, open the bell, click the gear → prefs panel shows per-type mute/sound switches; toggling mute disables the sound switch; toggles persist across a reopen (PUT round-trip). Toggle gear again → back to the list.

- [ ] **Step 7: Commit**

```bash
git add ui/src/components/notification-bell.tsx ui/src/i18n/locales/en.json ui/src/i18n/locales/ru.json
git commit -m "feat(notifications): bell gear + per-type mute/sound prefs panel"
```

---

## Task 6: Verify + deploy

**Files:** none.

- [ ] **Step 1: Full local verify**

Run: `make check` (or `cargo check`), `cargo clippy --all-targets -- -D warnings` (deploy gate), `cd ui && npm run build`.

- [ ] **Step 2: Push + server sqlx**

Push `master`. On the server, pull and run the throttled scoped test (the migration 085 runs automatically for `#[sqlx::test]` via the `migrations` attr):

```bash
ssh <server> 'cd ~/opex-src && git pull --ff-only && . ~/.cargo/env && \
  DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  CARGO_BUILD_JOBS=4 nice -n 19 ionice -c3 cargo test -p opex-db notification_prefs:: -- --nocapture'
```

Expected: `prefs_upsert_and_read` PASS.

- [ ] **Step 3: Deploy** (user-confirmed production deploy)

Backend: `ssh <server> 'bash ~/opex-src/scripts/server-deploy.sh'` (release build + swap + restart; **migration 085 runs on startup** — confirm the deploy log shows `synced NN files (latest: 085_notification_prefs.sql)` and `migrations complete`). UI: `bash scripts/deploy-ui.sh`. Health: `/health` ok, services active.

- [ ] **Step 4: Protocol E2E (live, token server-side)**

Verify muted enforcement end-to-end without a browser: PUT `{"type":"agent_error","muted":true,"sound":false}`; open a WS; POST a `agent_error` notification → assert **NO** `notification` frame arrives on the WS (muted → not broadcast) but `GET /api/notifications` **does** include it (still persisted). Then PUT `{"type":"agent_error","muted":false,"sound":true}`; POST again → the `notification` frame **does** arrive. Clean up (mark the test rows read; reset the pref to defaults).

- [ ] **Step 5: Manual browser check**

Bell gear → mute a type → trigger it → no live bell/sound, but it appears on refetch. Unmute → live again.

---

## Self-Review

**Spec coverage (§6):**
- `notification_prefs` table (type PK, muted, sound, updated_at) → Task 1. *Deviation from spec:* the spec listed a `push` column; Phase 3 omits it (Web Push is Phase 5, and adding the column then is a trivial migration). Only `muted`/`sound` are wired now.
- `GET`/`PUT /api/notification-prefs` → Task 2.
- `notify()` reads prefs; muted → persist + skip broadcast/sound → Task 3 (broadcast skip) + Task 4 (sound gating client-side). `sound=false` → broadcast but client suppresses beep → Task 4 WS gating.
- Gear panel in bell header with per-type toggles → Task 5.

**Placeholder scan:** none — every step has concrete code.

**Type consistency:** `is_muted(db, &str) -> Result<bool>` / `list_prefs -> Vec<NotificationPref>` / `upsert_pref(db, &str, bool, bool)` (Task 1) are called with those exact types in Tasks 2-3. The `{type, muted, sound}` PUT body (Task 2) matches the frontend `NotificationPref` interface and `useUpdateNotificationPref` payload (Task 4) and the panel's `updatePref.mutate({type, muted, sound})` (Task 5). `prependNotification(row, silent?)` (Task 4) is called with the silent flag by the WS handler (Task 4) and without it everywhere else (backward-compatible default `false`).

**Phase 1/2 invariants:** the `silent` flag only gates `newArrivalSeq` (the sound trigger); it still adds the row and bumps `unread_count`, so the badge stays correct. Muted types never broadcast, so they don't reach `prependNotification` at all — they surface via the Phase 2 `syncFirstPage` refetch (counted, in history), consistent with the documented muted semantics. `syncFirstPage`/`appendOlder`/read-reconcilers are untouched.
