# File Handlers Tools tab + legacy FSE retirement — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "File Handlers" (Обработчики файлов) tab under Tools that lists the File Handler Hub's handlers with a builtin-allowlist toggle, and fully remove the legacy File Scenario Engine (FSE) — UI + backend — keeping only the parts the hub shares.

**Architecture:** Three phases. **A** is purely additive: a new `handlers_admin.rs` backend module (`GET /api/handlers`, `GET/PUT /api/handlers/allowlist`) over the SAME allowlist store the composer reads, plus a third tab in `tools/page.tsx`. **B** removes the legacy FSE frontend (the `/file-scenarios` page, its nav entry, queries, types, i18n, and the live `file-scenario-chips` chat-store code). **C** removes the legacy FSE backend (enrich sync-dispatch, chips SSE wire, Telegram `fse:` callback, the `file_scenario` agent tool, `/api/file-scenarios/*` routes, the DB module, the module tree, the startup seeder, dead validators) and adds a non-destructive deprecate migration. The order A→B→C keeps the allowlist reachable throughout (its logic moves from the deleted route to the new one before the old route is removed).

**Tech Stack:** Rust 2024 (axum 0.8, sqlx 0.8, serde) for core + opex-types; Next.js 16 / React 19 / TanStack Query / Zustand / shadcn / vitest for the UI; PostgreSQL 17 migrations (sqlx).

## Global Constraints

- **Spec of record:** `docs/superpowers/specs/2026-07-01-file-handlers-tab-and-fse-retirement-design.md`. This plan supersedes the spec where they disagree; the errata below are already folded in.
- **Errata vs spec (verified against the live tree 2026-07-01 — use these values):**
  - The TS-codegen target is **`make gen-types`** (= `cargo run --features ts-gen --bin gen_ts_types -p opex-core`), NOT `make gen-ts` (the spec's `gen-ts` target does not exist).
  - `FSE_DEFAULT_ALLOWLIST` has **5** members: `["transcribe", "describe", "extract_document", "save", "summarize_video"]` (the legacy frontend helper listed only 4 — that helper is deleted).
  - `owner_gate.rs` lives at `agent/file_scenario/owner_gate.rs` and is **legacy-only** (its only non-test caller is the deleted Telegram callback).
  - The `file_scenario` tool schema + pin tests are in `agent/pipeline/tool_defs.rs` (there is no `agent/tool_defs.rs`).
- **rustls only** — never introduce OpenSSL. All HTTP via existing `reqwest` clients.
- **Per-task gate:** `make check` (`cargo check --all-targets`) must pass at the end of every task. **Phase-end gate (after C8):** `make lint` (`cargo clippy --all-targets -- -D warnings`) clean, `cd ui && npm test` green, `cd ui && npm run build` clean, and a **grep gate** confirming zero residual references to the deleted symbols (list in C8).
- **DB-backed tests** (`#[sqlx::test]`) need a live Postgres + `DATABASE_URL`; run them with `make test-db` (boots isolated postgres on :5434). Without a DB they fail with `EnvVar(NotPresent)` — that is expected locally; the CI/`make test-db` path is authoritative.
- **Allowlist single-store invariant:** the toggle MUST go through `crate::agent::fse::{get_enabled_allowlist, set_enabled_allowlist}` (which read/write `system_flags['fse.allowlist.enabled']`). Never reference the raw key (it is a private const).
- **No Co-Authored-By** trailer in commits. Work on `master`.
- **DRY / YAGNI / TDD / frequent commits.** One commit per task minimum.

---

## File Structure

**Phase A (additive):**
- Create `crates/opex-core/src/gateway/handlers/handlers_admin.rs` — the admin endpoints (list + allowlist GET/PUT). One responsibility: read-only manifest listing + builtin-allowlist toggle over the shared store.
- Modify `crates/opex-core/src/gateway/handlers/mod.rs` — declare the module.
- Modify `crates/opex-core/src/gateway/mod.rs` — merge the routes.
- Modify `ui/src/types/api.ts`, `ui/src/lib/queries.ts`, `ui/src/app/(authenticated)/tools/page.tsx`, `ui/src/i18n/locales/{en,ru}.json` — the tab.

**Phase B (frontend removal):** delete `ui/src/app/(authenticated)/file-scenarios/` (whole dir); edit `ui/src/components/app-sidebar.tsx`, `ui/src/lib/queries.ts`, `ui/src/types/api.ts`, `ui/src/i18n/locales/{en,ru}.json`, `ui/src/stores/stream/stream-processor.ts`, `ui/src/stores/chat-types.ts`; delete the chips test/fixture files.

**Phase C (backend removal):** edit `subagent.rs`, `bootstrap.rs`, `engine/run.rs`, `execute.rs`, `stream_event.rs`, `coalescer.rs`, `sse_writer.rs`, `sse_converter.rs`, `opex-types/src/sse.rs`, `dto_export/sse_ts.rs`, `channel_ws/{inline,reader}.rs`, `tool_handlers/mod.rs`, `pipeline/tool_defs.rs`, `db/mod.rs`, `gateway/mod.rs`, `handlers/mod.rs`, `agent/file_scenario/mod.rs`, `agent/fse/{mod,allowlist}.rs`, `agent/mod.rs`, `lib.rs`, `main.rs`; delete `agent/file_scenario/{dispatch,dispatch_seam,rewrite,sniff,owner_gate}.rs`, `agent/fse/seeder.rs`, `db/file_scenarios.rs`, `gateway/handlers/file_scenarios/` (dir), `opex-types/tests/sse_wire.rs` (edit), the 4 `tests/integration_fse_*.rs`; create `migrations/069_fse_deprecate.sql`.

---

# Phase A — New "File Handlers" tab (additive)

### Task A1: Backend admin endpoints (`/api/handlers`, `/api/handlers/allowlist`)

**Files:**
- Create: `crates/opex-core/src/gateway/handlers/handlers_admin.rs`
- Modify: `crates/opex-core/src/gateway/handlers/mod.rs` (after the `files` decl, ~line 36)
- Modify: `crates/opex-core/src/gateway/mod.rs` (merge chain, after `files::routes()`, ~line 82 region / real line 66-67)

**Interfaces:**
- Consumes: `crate::agent::fse::{get_enabled_allowlist, set_enabled_allowlist, is_allowed_for_autorun, FSE_DEFAULT_ALLOWLIST}`; `crate::agent::handler_registry::{HandlerManifest, HandlerMatch, HandlerRegistry}` (all already `pub`); `crate::gateway::clusters::InfraServices`, `crate::gateway::AppState` (both `FromRef<AppState>` already implemented); `crate::db::audit::{audit_spawn, event_types::FSE_ALLOWLIST_AMENDED}`.
- Produces: `pub(crate) fn routes() -> Router<AppState>`; JSON `GET /api/handlers → { handlers: HandlerAdminRow[] }` and `GET/PUT /api/handlers/allowlist → { allowlist: [{action_ref, enabled}] }`. The frontend types in Task A2 mirror `HandlerAdminRow`.

- [ ] **Step 1: Write the failing unit test for `HandlerAdminRow::from_manifest`**

Add to the bottom of the new file (create the file with just this test first, plus a stub `HandlerAdminRow` + `from_manifest`, so the test compiles-then-fails on the assertion). The test pins the enabled-derivation rule (builtin → allowlist-gated; workspace → always true):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::handler_registry::{HandlerManifest, HandlerMatch};

    fn manifest(id: &str, tier: &str) -> HandlerManifest {
        HandlerManifest {
            id: id.to_string(),
            labels: Default::default(),
            descriptions: Default::default(),
            icon: String::new(),
            match_: HandlerMatch::default(),
            capability: None,
            provider: None,
            execution: "sync".to_string(),
            output: String::new(),
            params: serde_json::Value::Null,
            order: 0,
            tier: tier.to_string(),
        }
    }

    #[test]
    fn builtin_enabled_follows_allowlist() {
        let enabled = vec!["transcribe".to_string()];
        let on = HandlerAdminRow::from_manifest(&manifest("transcribe", "builtin"), &enabled);
        let off = HandlerAdminRow::from_manifest(&manifest("describe", "builtin"), &enabled);
        assert!(on.enabled, "allowlisted builtin must be enabled");
        assert!(!off.enabled, "non-allowlisted builtin must be disabled");
    }

    #[test]
    fn workspace_always_enabled() {
        let row = HandlerAdminRow::from_manifest(&manifest("my_handler", "workspace"), &[]);
        assert!(row.enabled, "workspace handlers are never allowlist-gated");
    }

    #[test]
    fn non_member_is_rejected_by_membership_guard() {
        assert!(!is_allowlist_member("code_exec"));
        assert!(is_allowlist_member("transcribe"));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p opex-core handlers_admin -- --nocapture`
Expected: FAIL — `HandlerAdminRow` / `from_manifest` / `is_allowlist_member` not defined (compile error) until Step 3.

- [ ] **Step 3: Create `handlers_admin.rs` (full module)**

Create `crates/opex-core/src/gateway/handlers/handlers_admin.rs` with the following (the `#[cfg(test)] mod tests` from Step 1 goes at the very bottom):

```rust
//! Admin surface for the File Handler Hub — the "File Handlers" (Обработчики
//! файлов) tab under Tools. Read-only manifest listing + the builtin allowlist
//! toggle. The allowlist is the SAME single store the composer's
//! `/api/files/{id}/actions` reads (`system_flags['fse.allowlist.enabled']`
//! via `get_enabled_allowlist`), so toggling here changes which builtin
//! buttons appear per-file. Behind bearer auth (merged in `gateway/mod.rs`);
//! not loopback-exempt.

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::fse::{
    get_enabled_allowlist, is_allowed_for_autorun, set_enabled_allowlist, FSE_DEFAULT_ALLOWLIST,
};
use crate::agent::handler_registry::{HandlerManifest, HandlerRegistry};
use crate::gateway::AppState;
use crate::gateway::clusters::InfraServices;

// ── Response / request types ───────────────────────────────────────────────────

/// One handler row for the admin tab: the toolgate manifest plus the derived
/// `enabled` flag (builtin → allowlist-gated; workspace → always true).
/// `params` is intentionally omitted (the admin tab renders no param schema).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HandlerAdminRow {
    pub id: String,
    pub labels: std::collections::HashMap<String, String>,
    pub descriptions: std::collections::HashMap<String, String>,
    pub icon: String,
    #[serde(rename = "match")]
    pub match_: crate::agent::handler_registry::HandlerMatch,
    pub capability: Option<String>,
    pub provider: Option<String>,
    pub execution: String,
    pub output: String,
    pub order: i32,
    pub tier: String,
    pub enabled: bool,
}

impl HandlerAdminRow {
    fn from_manifest(m: &HandlerManifest, enabled_allowlist: &[String]) -> Self {
        let enabled = if m.tier == "builtin" {
            is_allowed_for_autorun(&m.id, enabled_allowlist)
        } else {
            true
        };
        Self {
            id: m.id.clone(),
            labels: m.labels.clone(),
            descriptions: m.descriptions.clone(),
            icon: m.icon.clone(),
            match_: m.match_.clone(),
            capability: m.capability.clone(),
            provider: m.provider.clone(),
            execution: m.execution.clone(),
            output: m.output.clone(),
            order: m.order,
            tier: m.tier.clone(),
            enabled,
        }
    }
}

/// Body for `PUT /api/handlers/allowlist` — toggles one builtin member.
#[derive(Debug, Deserialize)]
pub(crate) struct SetAllowlistBody {
    pub action_ref: String,
    pub enabled: bool,
}

/// Closed-domain check: only a member of the hard-coded `FSE_DEFAULT_ALLOWLIST`
/// may be toggled (can never admit `code_exec` / a YAML tool). Mirrors the
/// legacy `file_scenarios::is_allowlist_member`.
fn is_allowlist_member(name: &str) -> bool {
    FSE_DEFAULT_ALLOWLIST.contains(&name)
}

// ── Routes ───────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/handlers", get(api_list_handlers))
        .route(
            "/api/handlers/allowlist",
            get(api_get_allowlist).put(api_set_allowlist),
        )
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /api/handlers` → `{ handlers: [HandlerAdminRow...] }`. Lists ALL
/// registered manifests (no upload needed), each annotated with `enabled`.
/// Fail-soft: `refresh()` keeps stale/empty cache on toolgate error → an empty
/// list is returned (the tab shows an empty-state), never a 500.
async fn api_list_handlers(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
) -> impl IntoResponse {
    handlers.refresh().await;
    let manifests = handlers.manifests().await;
    let enabled = get_enabled_allowlist(&infra.db).await;
    let mut rows: Vec<HandlerAdminRow> = manifests
        .iter()
        .map(|m| HandlerAdminRow::from_manifest(m, &enabled))
        .collect();
    rows.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    Json(json!({ "handlers": rows })).into_response()
}

/// `GET /api/handlers/allowlist` → the 5 const members + enabled state.
/// Wrapper over `get_enabled_allowlist` — same store as the composer.
async fn api_get_allowlist(State(infra): State<InfraServices>) -> impl IntoResponse {
    let enabled_set = get_enabled_allowlist(&infra.db).await;
    let members: Vec<serde_json::Value> = FSE_DEFAULT_ALLOWLIST
        .iter()
        .map(|m| {
            let is_enabled = enabled_set.iter().any(|e| e == m);
            json!({ "action_ref": m, "enabled": is_enabled })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "allowlist": members }))).into_response()
}

/// `PUT /api/handlers/allowlist` body `{action_ref, enabled}` → toggle one
/// builtin member via `set_enabled_allowlist` (const-validated). Non-member →
/// 400. Audits `FSE_ALLOWLIST_AMENDED` on success (preserves the legacy trail).
async fn api_set_allowlist(
    State(infra): State<InfraServices>,
    Json(body): Json<SetAllowlistBody>,
) -> impl IntoResponse {
    if !is_allowlist_member(&body.action_ref) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "'{}' is not a member of the allowlist; only {} may be toggled",
                    body.action_ref,
                    FSE_DEFAULT_ALLOWLIST.join(", ")
                )
            })),
        )
            .into_response();
    }

    let mut current = get_enabled_allowlist(&infra.db).await;
    if body.enabled {
        if !current.iter().any(|m| m == &body.action_ref) {
            current.push(body.action_ref.clone());
        }
    } else {
        current.retain(|m| m != &body.action_ref);
    }

    match set_enabled_allowlist(&infra.db, &current).await {
        Ok(()) => {
            crate::db::audit::audit_spawn(
                infra.db.clone(),
                String::new(),
                crate::db::audit::event_types::FSE_ALLOWLIST_AMENDED,
                Some("ui".into()),
                json!({ "action_ref": body.action_ref, "enabled": body.enabled }),
            );
            (
                StatusCode::OK,
                Json(json!({ "action_ref": body.action_ref, "enabled": body.enabled })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
```

> If a `use`/type path mismatches (e.g. `InfraServices` module path), mirror the exact imports of the existing `crates/opex-core/src/gateway/handlers/file_scenarios/mod.rs` — it uses the same `State<InfraServices>` extraction. Keep `Serialize`/`Deserialize`/`json` as above.

- [ ] **Step 4: Declare the module in `handlers/mod.rs`**

In `crates/opex-core/src/gateway/handlers/mod.rs`, after `pub(crate) mod files;` (currently followed by `pub(crate) mod clarify;`):

```rust
pub(crate) mod files;
pub(crate) mod handlers_admin;
pub(crate) mod clarify;
```

- [ ] **Step 5: Merge the routes in `gateway/mod.rs`**

In `crates/opex-core/src/gateway/mod.rs`, in the `Router::new()...merge(...)` chain, insert after the `handlers::files::routes()` line and before `handlers::llm::routes()`:

```rust
        .merge(handlers::files::routes())           // /api/files/{upload_id}/actions + /run
        .merge(handlers::handlers_admin::routes())  // /api/handlers, /api/handlers/allowlist (File Handlers tab)
        .merge(handlers::llm::routes());            // /api/llm/complete (raw LLM, auth-required)
```

(Auth is a single global `.layer` over the whole merged app — merging is sufficient to put these routes behind bearer auth; no per-route wiring.)

- [ ] **Step 6: Run the unit tests to verify they pass**

Run: `cargo test -p opex-core handlers_admin -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 7: Add the DB-backed allowlist round-trip test (proves the "same store" link)**

Append to `#[cfg(test)] mod tests` in `handlers_admin.rs`:

```rust
    #[sqlx::test]
    async fn allowlist_toggle_round_trips_through_shared_store(pool: sqlx::PgPool) {
        use crate::agent::fse::{get_enabled_allowlist, set_enabled_allowlist};
        // Start from a known state: only "transcribe" enabled.
        set_enabled_allowlist(&pool, &["transcribe".to_string()])
            .await
            .unwrap();
        let got = get_enabled_allowlist(&pool).await;
        assert_eq!(got, vec!["transcribe".to_string()]);
        // A HandlerAdminRow for a builtin reflects that store exactly.
        let m = HandlerManifest {
            id: "describe".to_string(),
            labels: Default::default(),
            descriptions: Default::default(),
            icon: String::new(),
            match_: crate::agent::handler_registry::HandlerMatch::default(),
            capability: None,
            provider: None,
            execution: "sync".to_string(),
            output: String::new(),
            params: serde_json::Value::Null,
            order: 0,
            tier: "builtin".to_string(),
        };
        let row = HandlerAdminRow::from_manifest(&m, &get_enabled_allowlist(&pool).await);
        assert!(!row.enabled, "describe is not in the enabled set → disabled");
    }
```

- [ ] **Step 8: Run the DB test + full check**

Run: `make check` then `make test-db` (or `DATABASE_URL=... cargo test -p opex-core handlers_admin`)
Expected: `make check` clean; the `#[sqlx::test]` passes under `make test-db` (skipped without a DB — that is acceptable locally).

- [ ] **Step 9: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/handlers_admin.rs \
        crates/opex-core/src/gateway/handlers/mod.rs \
        crates/opex-core/src/gateway/mod.rs
git commit -m "feat(handlers): add /api/handlers + /api/handlers/allowlist admin endpoints"
```

---

### Task A2: Frontend "File Handlers" tab

**Files:**
- Modify: `ui/src/types/api.ts` (add types after the hub types, ~line 537)
- Modify: `ui/src/lib/queries.ts` (add qk keys ~line 93; add 3 hooks; add type imports)
- Modify: `ui/src/app/(authenticated)/tools/page.tsx` (imports, data hooks, TabsTrigger, TabsContent, `renderHandlerCard`)
- Modify: `ui/src/i18n/locales/en.json` + `ui/src/i18n/locales/ru.json` (12 `tools.*` keys)
- Test: `ui/src/app/(authenticated)/tools/__tests__/handlers-tab.test.tsx` (create)

**Interfaces:**
- Consumes: `GET /api/handlers` / `GET,PUT /api/handlers/allowlist` from Task A1; `useLanguageStore((s) => s.locale)` from `@/stores/language-store`; `Switch` from `@/components/ui/switch`; `Row`/`TypeBadge` from `./ToolHelpers`; `EmptyState`.
- Produces: `HandlerAdminRow`, `HandlerAllowlistRow` types; `useHandlers`, `useHandlerAllowlist`, `useSetHandlerAllowlist` hooks.

- [ ] **Step 1: Add the API types**

In `ui/src/types/api.ts`, after the `FileActionsResponse` interface (~line 537), add:

```ts
// ── File Handlers admin (Tools tab) ────────────────────────────────────────────
// Source: crates/opex-core/src/gateway/handlers/handlers_admin.rs
// GET /api/handlers → { handlers: HandlerAdminRow[] }

export interface HandlerAdminRow {
  id: string;
  labels: Record<string, string>;        // { ru, en }
  descriptions: Record<string, string>;  // { ru, en }
  icon: string;
  match: { mime?: string[]; max_size_mb?: number };
  capability?: string | null;
  provider?: string | null;
  execution: "sync" | "async";
  output: string;
  order: number;
  tier: "builtin" | "workspace";
  enabled: boolean;
}

/** One entry from GET /api/handlers/allowlist — the 5 FSE_DEFAULT_ALLOWLIST members. */
export interface HandlerAllowlistRow {
  action_ref: string;
  enabled: boolean;
}
```

- [ ] **Step 2: Add query keys + hooks in `queries.ts`**

Add the query keys near the existing `qk` object (~line 93):

```ts
  handlers: ["handlers"] as const,
  handlerAllowlist: ["handlers", "allowlist"] as const,
```

Add `HandlerAdminRow, HandlerAllowlistRow` to the `@/types/api` type-import block. Then add a new section (e.g. after the yaml/mcp hooks):

```ts
// ── File Handlers ────────────────────────────────────────────────────────────

export function useHandlers() {
  return useQuery({
    queryKey: qk.handlers,
    queryFn: () => apiGet<{ handlers: HandlerAdminRow[] }>("/api/handlers"),
    select: (d) => d.handlers,
    staleTime: 30_000,
  })
}

export function useHandlerAllowlist() {
  return useQuery({
    queryKey: qk.handlerAllowlist,
    queryFn: () => apiGet<{ allowlist: HandlerAllowlistRow[] }>("/api/handlers/allowlist"),
    select: (d) => d.allowlist,
    staleTime: 30_000,
  })
}

export function useSetHandlerAllowlist() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: { action_ref: string; enabled: boolean }) =>
      apiPut<{ action_ref: string; enabled: boolean }>("/api/handlers/allowlist", data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: qk.handlerAllowlist })
      qc.invalidateQueries({ queryKey: qk.handlers }) // handlers carry the merged `enabled`
    },
    onError: (e: Error) => toast.error(e.message),
  })
}
```

- [ ] **Step 3: Write the failing vitest for the tab**

Create `ui/src/app/(authenticated)/tools/__tests__/handlers-tab.test.tsx`. Mock the queries module so the tab renders deterministic rows and the toggle calls the mutation:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

const mutate = vi.fn();
vi.mock("@/lib/queries", () => ({
  qk: {},
  useYamlTools: () => ({ data: [], isLoading: false, error: null }),
  useMcpServers: () => ({ data: [], isLoading: false, error: null }),
  useHandlers: () => ({
    data: [
      { id: "transcribe", labels: { en: "Transcribe" }, descriptions: { en: "STT" },
        icon: "mic", match: { mime: ["audio/*"] }, capability: "stt", provider: "Whisper",
        execution: "async", output: "text", order: 10, tier: "builtin", enabled: true },
      { id: "my_handler", labels: { en: "My Handler" }, descriptions: {},
        icon: "", match: {}, execution: "sync", output: "text", order: 20,
        tier: "workspace", enabled: true },
    ],
    isLoading: false, error: null,
  }),
  useHandlerAllowlist: () => ({ data: [] }),
  useSetHandlerAllowlist: () => ({ mutate, isPending: false }),
}));

import ToolsPage from "../page";

describe("File Handlers tab", () => {
  beforeEach(() => mutate.mockClear());

  it("renders a card per handler and toggles a builtin via PUT", async () => {
    render(<ToolsPage />);
    // Switch to the handlers tab.
    fireEvent.click(screen.getByRole("tab", { name: /File Handlers|Обработчики/i }));
    expect(await screen.findByText("Transcribe")).toBeInTheDocument();
    expect(screen.getByText("My Handler")).toBeInTheDocument();
    // The builtin has a Switch; toggling it fires the mutation with its id.
    const toggle = screen.getByLabelText("transcribe");
    fireEvent.click(toggle);
    expect(mutate).toHaveBeenCalledWith({ action_ref: "transcribe", enabled: false });
  });
});
```

Run: `cd ui && npx vitest run src/app/\(authenticated\)/tools/__tests__/handlers-tab.test.tsx`
Expected: FAIL — the `handlers` tab / `renderHandlerCard` do not exist yet.

- [ ] **Step 4: Wire the tab in `tools/page.tsx` — imports + language + data hooks**

Add imports: `import { Switch } from "@/components/ui/switch";`, `import { useLanguageStore } from "@/stores/language-store";`, add `FileCog` to the `lucide-react` import list, add `useHandlers, useHandlerAllowlist, useSetHandlerAllowlist` to the `@/lib/queries` import, add `HandlerAdminRow` to the `@/types/api` import.

Inside the component, add the language + data hooks alongside the existing `useYamlTools`/`useMcpServers`:

```tsx
  const lang = useLanguageStore((s) => s.locale);
  const { data: handlers = [], isLoading: handlersLoading, error: handlersError } = useHandlers();
  const setHandlerAllowlist = useSetHandlerAllowlist();

  const loading = yamlLoading2 || mcpLoading || handlersLoading;
  const errorMsg = yamlError ? String(yamlError) : mcpError ? String(mcpError) : handlersError ? String(handlersError) : "";
```

(`useHandlerAllowlist` is available if a separate allowlist view is wanted; the card toggle uses `handlers[].enabled` directly, so it is optional to call here.)

- [ ] **Step 5: Add `renderHandlerCard`**

Define alongside `renderYamlCard`/`renderMcpCard` (before the tabbed view):

```tsx
  const renderHandlerCard = (h: HandlerAdminRow) => {
    const label = h.labels?.[lang] ?? h.labels?.en ?? h.id;
    const description = h.descriptions?.[lang] ?? h.descriptions?.en ?? "";
    const isBuiltin = h.tier === "builtin";
    const pending = setHandlerAllowlist.isPending;
    return (
      <div key={`handler-${h.id}`}
        className={`flex flex-col gap-3 neu-flat p-5 min-w-0 overflow-hidden ${isBuiltin && !h.enabled ? "opacity-50" : ""}`}>
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3 min-w-0">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border bg-accent/50 border-border">
              <FileCog className="h-4 w-4 text-foreground/70" />
            </div>
            <span className="font-mono text-sm font-bold text-foreground break-words leading-snug min-w-0" title={h.id}>{label}</span>
          </div>
          <div className="flex flex-col items-end gap-1 shrink-0">
            <TypeBadge type={isBuiltin ? "INT" : "EXT"} />
            <Badge variant="secondary" className="text-[10px]">
              {h.execution === "async" ? t("tools.handler_async") : t("tools.handler_sync")}
            </Badge>
          </div>
        </div>
        {description && (
          <p className="text-xs text-muted-foreground line-clamp-2">{description}</p>
        )}
        <div className="space-y-1.5 mt-auto text-xs">
          <Row label={t("tools.handler_tier")} value={isBuiltin ? t("tools.handler_builtin") : t("tools.handler_workspace")} />
          {h.match?.mime?.length ? (
            <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 overflow-hidden">
              <span className="text-muted-foreground">{t("tools.handler_mime")}</span>
              <span className="font-mono text-primary/70 truncate" title={h.match.mime.join(", ")}>{h.match.mime.join(", ")}</span>
            </div>
          ) : null}
          {h.provider && (
            <Row label={t("tools.handler_provider")} value={h.provider} />
          )}
        </div>
        <div className="flex items-center justify-between pt-1">
          {isBuiltin ? (
            <Switch
              aria-label={h.id}
              checked={h.enabled}
              disabled={pending}
              onCheckedChange={(v) => setHandlerAllowlist.mutate({ action_ref: h.id, enabled: v })}
            />
          ) : (
            <Badge variant="secondary" className="text-[10px]">{t("tools.handler_always_on")}</Badge>
          )}
        </div>
      </div>
    );
  };
```

Add `Row` to the `./ToolHelpers` import if not already imported.

- [ ] **Step 6: Add the TabsTrigger + TabsContent**

In the `<TabsList>` add a third trigger after the `mcp` trigger:

```tsx
              <TabsTrigger value="handlers">
                <FileCog className="h-3.5 w-3.5" />
                {t("tools.file_handlers")}
                <Badge variant="secondary" className="ml-1.5 text-[10px]">{handlers.length}</Badge>
              </TabsTrigger>
```

After the MCP `</TabsContent>` (before `</Tabs>`) add:

```tsx
            {/* ── File Handlers ── */}
            <TabsContent value="handlers" className="mt-6">
              {handlers.length === 0 ? (
                <EmptyState
                  icon={FileCog}
                  text={t("tools.no_handlers")}
                  hint={
                    <a href="/workspace/" className="mt-3 text-xs text-primary hover:underline">
                      {t("tools.add_handler")}
                    </a>
                  }
                />
              ) : (
                <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
                  {handlers.map((h) => renderHandlerCard(h))}
                </div>
              )}
            </TabsContent>
```

- [ ] **Step 7: Add i18n keys (en.json + ru.json)**

In `ui/src/i18n/locales/en.json`, after `"tools.reload": "Reload",` (keeping flat dotted keys):

```json
  "tools.file_handlers": "File Handlers",
  "tools.no_handlers": "No file handlers registered. Handlers come from toolgate builtins and workspace/file_handlers/*.py.",
  "tools.add_handler": "Add your own handler →",
  "tools.handler_builtin": "builtin",
  "tools.handler_workspace": "workspace",
  "tools.handler_tier": "Tier",
  "tools.handler_sync": "sync",
  "tools.handler_async": "async",
  "tools.handler_mime": "MIME",
  "tools.handler_provider": "Provider",
  "tools.handler_always_on": "always on",
```

In `ui/src/i18n/locales/ru.json`, after `"tools.reload": "Перечитать",`:

```json
  "tools.file_handlers": "Обработчики файлов",
  "tools.no_handlers": "Нет зарегистрированных обработчиков файлов. Обработчики — встроенные из toolgate и workspace/file_handlers/*.py.",
  "tools.add_handler": "Добавить свой обработчик →",
  "tools.handler_builtin": "встроенный",
  "tools.handler_workspace": "workspace",
  "tools.handler_tier": "Тип",
  "tools.handler_sync": "синхронный",
  "tools.handler_async": "асинхронный",
  "tools.handler_mime": "MIME",
  "tools.handler_provider": "Провайдер",
  "tools.handler_always_on": "всегда включён",
```

(`TranslationKey` = `keyof typeof ru` (auto-derived) — adding keys to both JSONs extends the type with no manual edit. Add the SAME keys to both files or tsc breaks.)

- [ ] **Step 8: Run the vitest to verify it passes + full suite**

Run: `cd ui && npx vitest run src/app/\(authenticated\)/tools/__tests__/handlers-tab.test.tsx` → PASS.
Then `cd ui && npm test` → green; `cd ui && npm run build` → clean (no unused-import lint).

- [ ] **Step 9: Commit**

```bash
git add ui/src/types/api.ts ui/src/lib/queries.ts \
        "ui/src/app/(authenticated)/tools/page.tsx" \
        "ui/src/app/(authenticated)/tools/__tests__/handlers-tab.test.tsx" \
        ui/src/i18n/locales/en.json ui/src/i18n/locales/ru.json
git commit -m "feat(ui): add File Handlers tab to /tools (list + builtin allowlist toggle)"
```

---

# Phase B — Legacy FSE frontend removal

### Task B1: Remove the `/file-scenarios` page, nav, queries, types, i18n

**Files:**
- Delete: `ui/src/app/(authenticated)/file-scenarios/` (whole dir: `page.tsx`, `AllowlistEditor.tsx`, `ScenarioRow.tsx`, `ScenarioDialog.tsx`, `_parts/helpers.ts`, `_parts/__tests__/helpers.test.ts`, `__tests__/{page,ScenarioRow,ScenarioDialog,AllowlistEditor}.test.tsx`)
- Modify: `ui/src/components/app-sidebar.tsx` (icon import ~line 29; nav entry ~line 77)
- Modify: `ui/src/lib/queries.ts` (7 FS hooks ~699-767; 2 qk keys ~92-93; type imports)
- Modify: `ui/src/types/api.ts` (FS/ScenarioChoice block ~464-520)
- Modify: `ui/src/i18n/locales/en.json` + `ru.json` (file_scenarios.* block + nav.file_scenarios ~1194-1218)
- Delete: `ui/src/lib/__tests__/file-scenarios-queries.test.ts`

**Interfaces:** none produced. Depends on Task A2 having shipped the new tab (the allowlist is now editable there).

- [ ] **Step 1: Delete the page directory + its query test**

```bash
git rm -r "ui/src/app/(authenticated)/file-scenarios"
git rm ui/src/lib/__tests__/file-scenarios-queries.test.ts
```

- [ ] **Step 2: Remove the sidebar nav entry + unused icon**

In `ui/src/components/app-sidebar.tsx`, remove `FileCog` from the `lucide-react` import (the `Archive, Monitor, FileCog,` line becomes `Archive, Monitor,`), and delete the nav entry line:

```tsx
      { labelKey: "nav.file_scenarios", href: "/file-scenarios/", icon: FileCog },
```

(Verify no other `FileCog` use remains in this file before removing the import.)

- [ ] **Step 3: Remove the FS hooks + query keys + type imports in `queries.ts`**

Delete the entire File Scenario Engine hooks section (`useFileScenarios`, `useCreateFileScenario`, `useUpdateFileScenario`, `useDeleteFileScenario`, `useSetFileScenarioDefault`, `useFileScenarioAllowlist`, `useSetFileScenarioAllowlist`, ~lines 699-767). Delete the two qk keys `fileScenarios` + `fileScenarioAllowlist` (~92-93). Remove `FileScenario, CreateFileScenarioInput, UpdateFileScenarioInput, FileScenarioAllowlistRow` from the `@/types/api` import block.

- [ ] **Step 4: Remove the FS types in `types/api.ts`**

Delete the `// ── File Scenario Engine (Phase 8) ──` section comment + `FileScenario` + `CreateFileScenarioInput` + `UpdateFileScenarioInput` + `FileScenarioAllowlistRow` + the hand-written `ScenarioChoice` interface (the contiguous block ~lines 464-520).

- [ ] **Step 5: Remove the i18n block (both files) — mind the trailing comma**

In BOTH `en.json` and `ru.json`, delete the contiguous `file_scenarios.*` block plus the terminal `"nav.file_scenarios": ...` key (~lines 1194-1218). `nav.file_scenarios` is the LAST key before the closing `}` — after deletion, ensure the new last key (`"login.show_token": ...` at ~line 1193) has **no** trailing comma, or the JSON is invalid.

- [ ] **Step 6: Verify build + suite**

Run: `cd ui && npm run build` (catches dangling imports / unused `FileCog` / broken JSON) then `cd ui && npm test`.
Expected: build clean; suite green (no test references the deleted page — `file-scenarios-queries.test.ts` was removed in Step 1).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(ui): remove legacy File Scenarios page, nav, queries, types, i18n"
```

---

### Task B2: Remove the live `file-scenario-chips` chat-store code + its tests

**Files:**
- Modify: `ui/src/stores/stream/stream-processor.ts` (import ~line 25; `case "file-scenario-chips"` ~356-366)
- Modify: `ui/src/stores/chat-types.ts` (`FileScenarioChipsPart` interface ~92-108 + its `MessagePart` union member)
- Delete: `ui/src/__tests__/fixtures/sse/file-scenario-chips.json`
- Delete: `ui/src/stores/stream/__tests__/fse-chips.test.ts`
- Delete: `ui/src/__tests__/sse-fse-codegen.test.ts`
- Modify: `ui/src/__tests__/sse-events.fixtures.test.ts` (remove the `file-scenario-chips` case ~178-226 AND fix the fixture-inventory test that hard-codes `"file-scenario-chips.json"` + the count `27`)

**Interfaces:** none. The generated `sse.generated.ts` still contains the chips type until Phase C regen; nothing narrows on it after this task, so tsc stays green.

- [ ] **Step 1: Remove the stream-processor case + import**

In `ui/src/stores/stream/stream-processor.ts`, delete the `FileScenarioChipsPart` import (~line 25) and the whole `case "file-scenario-chips": { ... }` block (~356-366).

- [ ] **Step 2: Remove the MessagePart variant**

In `ui/src/stores/chat-types.ts`, delete the `FileScenarioChipsPart` interface (~92-108) and its member in the `MessagePart` union.

- [ ] **Step 3: Delete the chips fixture + guard/codegen tests**

```bash
git rm ui/src/__tests__/fixtures/sse/file-scenario-chips.json
git rm ui/src/stores/stream/__tests__/fse-chips.test.ts
git rm ui/src/__tests__/sse-fse-codegen.test.ts
```

- [ ] **Step 4: Fix the fixture-inventory test**

In `ui/src/__tests__/sse-events.fixtures.test.ts`: remove the `file-scenario-chips` case (~178-226); in the "all N fixtures present" test remove `"file-scenario-chips.json"` from the expected Set and decrement the count in the title (e.g. `27` → `26`). Confirm the exact current count when editing (grep the test title).

- [ ] **Step 5: Verify suite**

Run: `cd ui && npm test`
Expected: green. No test asserts the chips symbols any more.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(ui): remove file-scenario-chips chat-store code + fixtures/tests"
```

---

# Phase C — Legacy FSE backend removal

> Order matters: each task ends with `make check` green. The sequence removes callers before definitions. `#[sqlx::test]` deletions are pure removals (no DB needed to delete).

### Task C1: Remove enrich sync-dispatch + affordance dataflow

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs` (`EnrichResult` ~183-198; `enrich_message_text` body ~236-291; enrich test ~982-1033; shape-guard test ~576-589 lives in bootstrap.rs — see below)
- Modify: `crates/opex-core/src/agent/pipeline/bootstrap.rs` (`AffordanceTransport` ~14-34; `BootstrapOutcome.pending_alternatives` ~66-69; enrich consumption ~270-290; emission block ~375-456; struct build ~520-537; `affordance_transport_for_channel` test ~544-553; `enrich_result_shape` test module ~576-589)
- Modify: `crates/opex-core/src/agent/engine/run.rs` (4 destructure/reconstruct pairs)
- Modify: `crates/opex-core/src/agent/pipeline/execute.rs` (destructure ~123-128)

**Interfaces:**
- Produces: `EnrichResult { text: String, video_accepted: bool }` (loses `outcomes`, `pending_alternatives`); `BootstrapOutcome` loses `pending_alternatives`. `ScenarioOutcome`/`ScenarioStatus` types are UNTOUCHED (toolgate wire type, MUST-STAY). The `detect_video_links → insert_handler_job` branch STAYS.

- [ ] **Step 1: Shrink `EnrichResult`**

In `subagent.rs`, replace the `EnrichResult` struct (~183-198) with:

```rust
/// Result of `enrich_message_text`: the enriched LLM text plus the async-video
/// short-circuit flag. (The legacy FSE per-attachment dispatch outcomes and
/// post-hoc chip alternatives were removed with the FSE sync-dispatch retirement.)
pub struct EnrichResult {
    pub text: String,
    /// `true` when this message was an async-video acceptance (a YouTube/Yandex
    /// Disk link was enqueued as a `summarize_video` handler job). The pipeline
    /// uses this to SHORT-CIRCUIT the LLM agent loop: the ack text is the whole
    /// reply, so the agent never tries to fetch/transcribe the link itself.
    pub video_accepted: bool,
}
```

- [ ] **Step 2: Remove the dispatch + rewrite calls in `enrich_message_text`**

In `subagent.rs`, from `enrich_with_attachments(&mut enriched, attachments);` onward, delete the `dispatch_attachments` call (~267-279), the `rewrite_enriched_text` call (~280-284), and the `let video_accepted = video_accepted || outcomes.iter()...` recompute (~286-288). The body ends with the URL-enqueue loop (kept) then:

```rust
    EnrichResult { text: enriched, video_accepted }
}
```

Update the doc comment (~200-202) to:

```rust
/// Enrich user text: auto-fetch URLs (max 2), add attachment hints, and enqueue a
/// `summarize_video` handler job for any detected video link. Returns an
/// `EnrichResult` carrying the enriched LLM text + the async-video short-circuit flag.
```

(Keep `mut video_accepted` — the enqueue loop mutates it. `http_client`/`gateway_listen`/`toolgate_url`/`db`/`session_id`/`agent_name`/`agent_language` params all stay used by the surviving URL-fetch + enqueue loops; keep `#[allow(clippy::too_many_arguments)]`.)

- [ ] **Step 3: Fix bootstrap.rs — remove `AffordanceTransport`, the field, the consumption, the emission, the struct build, and 2 tests**

In `crates/opex-core/src/agent/pipeline/bootstrap.rs`:
- Delete the `AffordanceTransport` enum + `affordance_transport()` helper + their doc (~14-34).
- Delete the `pending_alternatives` field + its doc + `#[allow(dead_code)]` in `BootstrapOutcome` (~66-69).
- Replace the enrich-consumption region (~270-290) with:

```rust
    let enriched_text = enrich.text;
    let video_accepted = enrich.video_accepted;
    // Clean user-facing ack for the short-circuit reply (never the whole enriched
    // blob, which carries PII-redacted text). The async-video accept always comes
    // from a detected video link now, so the ack is the canonical constant.
    let video_ack_text = if video_accepted {
        "🎬 Видео по ссылке принято, готовлю сводку.".to_string()
    } else {
        String::new()
    };
```

- Delete the entire "FSE affordance emission" block (~375-456, from the `// ── FSE affordance emission ──` comment through its closing brace).
- In the `Ok(BootstrapOutcome { ... })` construction (~520-537), delete the `pending_alternatives,` line.
- Delete the `affordance_transport_for_channel` test fn (~544-553).
- Delete the `enrich_result_shape` guard-test module (~576-589) — it constructs `EnrichResult` with the removed fields.

- [ ] **Step 4: Fix the 4 `engine/run.rs` sites**

In `crates/opex-core/src/agent/engine/run.rs`, at each of the 4 adapter sites, delete the `pending_alternatives: _,` destructure line AND the `pending_alternatives: vec![],` reconstruct line:
- SSE adapter (~96 destructure, ~125 reconstruct)
- channel adapter (~371, ~391)
- streaming adapter (~528, ~548)
- cron adapter (~724 destructure, ~768 reconstruct) — keep the `video_accepted: _,`/`video_ack_text: _,` ignore lines + their comment and the reconstruct defaults.

- [ ] **Step 5: Fix `execute.rs`**

In `crates/opex-core/src/agent/pipeline/execute.rs`, delete the `pending_alternatives: _,` line (~124) from the `BootstrapOutcome` destructure.

- [ ] **Step 6: Delete/rewrite the enrich unit test in subagent.rs**

Delete the `enrich_returns_enrichresult_and_strips_url_on_save` test (~982-1033) — it asserts `result.outcomes.len() == 1` (removed field) and its URL-strip behaviour depended on FSE dispatch. The `enrich_youtube_link_sets_video_accepted` and `enrich_plain_text_leaves_video_accepted_false` tests stay unchanged.

- [ ] **Step 7: Compile + run touched tests**

Run: `make check` then `cargo test -p opex-core pipeline::subagent -- --nocapture` and `cargo test -p opex-core pipeline::bootstrap -- --nocapture`
Expected: `make check` clean; the surviving enrich/bootstrap tests pass. (`StreamEvent::FileScenarioChips` still exists as a variant with its other consumers — removed in C2 — so this compiles.)

- [ ] **Step 8: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/subagent.rs \
        crates/opex-core/src/agent/pipeline/bootstrap.rs \
        crates/opex-core/src/agent/engine/run.rs \
        crates/opex-core/src/agent/pipeline/execute.rs
git commit -m "refactor(fse): remove enrich sync-dispatch + affordance chips dataflow"
```

---

### Task C2: Remove the chips SSE wire + regenerate TS types

**Files:**
- Modify: `crates/opex-core/src/agent/stream_event.rs` (variant ~74-82; test module ~138-160)
- Modify: `crates/opex-core/src/gateway/sse/coalescer.rs` (arm ~37)
- Modify: `crates/opex-core/src/gateway/handlers/chat/sse_writer.rs` (or-pattern ~86; method ~297-308; test ~683-699)
- Modify: `crates/opex-core/src/gateway/handlers/chat/sse_converter.rs` (arm ~468-477; guard test ~576-591)
- Modify: `crates/opex-types/src/sse.rs` (`SseEvent::FileScenarioChips` ~117-128; `ScenarioChoice` struct ~290-300)
- Modify: `crates/opex-types/tests/sse_wire.rs` (import ~11; `sse_file_scenario_chips_fixture` test ~302-329)
- Modify: `crates/opex-core/src/dto_export/sse_ts.rs` (import ~16; `register_ts_dto!(ScenarioChoice,…)` ~29)
- Regenerate: `ui/src/types/sse.generated.ts` (via `make gen-types`)

**Interfaces:** removes `StreamEvent::FileScenarioChips`, `SseEvent::FileScenarioChips`, `opex_types::sse::ScenarioChoice`, `build_file_scenario_chips`. Must land atomically (exhaustive matches over `StreamEvent`).

- [ ] **Step 1: Delete the `StreamEvent` variant + its test**

In `stream_event.rs`, delete the `FileScenarioChips { message_id, upload_id, alternatives }` variant + its doc (~74-82), and delete the whole `#[cfg(test)] mod fse_stream_event_tests` block (~138-160).

- [ ] **Step 2: Delete the coalescer arm**

In `coalescer.rs`, delete the arm `StreamEvent::FileScenarioChips { .. } => "file-scenario-chips",` (~37).

- [ ] **Step 3: Fix `sse_writer.rs`**

Remove the `| StreamEvent::FileScenarioChips { .. }` alternative from the `unimplemented!()` or-pattern (~86, so `RichCard` becomes the terminal pattern). Delete the `build_file_scenario_chips` method (~297-308). Delete the `writer_file_scenario_chips_emits_camelcase_wire` test (~683-699).

- [ ] **Step 4: Fix `sse_converter.rs`**

Delete the `StreamEvent::FileScenarioChips { ... } => { ... }` arm (~468-477) and the `#[cfg(test)] mod fse_converter_guard` block (~576-591, a source-scan test that would fail).

- [ ] **Step 5: Fix `opex-types/src/sse.rs`**

Delete the `SseEvent::FileScenarioChips { ... }` variant + doc (~117-128) and the `ScenarioChoice` struct + doc (~290-300).

- [ ] **Step 6: Fix `opex-types/tests/sse_wire.rs`**

Remove `ScenarioChoice, ` from the import (~11) and delete the `sse_file_scenario_chips_fixture` test fn (~302-329). (This test also wrote the UI fixture deleted in B2.)

- [ ] **Step 7: Un-register the TS DTO**

In `crates/opex-core/src/dto_export/sse_ts.rs`, remove `ScenarioChoice, ` from the `use opex_types::sse::{...}` import (~16) and delete the line `crate::register_ts_dto!(ScenarioChoice,       dest = "ui-sse");` (~29). This MUST precede the regen (Step 9) or the standalone type is re-emitted.

- [ ] **Step 8: Compile**

Run: `make check`
Expected: clean (opex-types + opex-core both compile; the variant and its consumers are gone).

- [ ] **Step 9: Regenerate the TS types**

Run: `make gen-types` (= `cargo run --features ts-gen --bin gen_ts_types -p opex-core`)
Then verify: `grep -n "ScenarioChoice\|file-scenario-chips" ui/src/types/sse.generated.ts` → zero hits.

- [ ] **Step 10: UI compiles clean**

Run: `cd ui && npm run build`
Expected: clean (the generated union no longer has the chips arm; nothing references it after B2).

- [ ] **Step 11: Commit**

```bash
git add crates/opex-core/src/agent/stream_event.rs \
        crates/opex-core/src/gateway/sse/coalescer.rs \
        crates/opex-core/src/gateway/handlers/chat/sse_writer.rs \
        crates/opex-core/src/gateway/handlers/chat/sse_converter.rs \
        crates/opex-types/src/sse.rs crates/opex-types/tests/sse_wire.rs \
        crates/opex-core/src/dto_export/sse_ts.rs ui/src/types/sse.generated.ts
git commit -m "refactor(fse): remove file-scenario-chips SSE wire + regenerate TS types"
```

---

### Task C3: Remove FSE integration + guard tests

**Files:**
- Delete: `crates/opex-core/tests/integration_fse_regression.rs`
- Delete: `crates/opex-core/tests/integration_fse_affordance.rs`
- Delete: `crates/opex-core/tests/integration_phase6_no_video_refs.rs`
- Modify (rewrite): `crates/opex-core/tests/integration_fse_security.rs`

**Interfaces:** none. Done early so later definition deletions compile (these tests reference `dispatch`/`rewrite`/`assert_fse_owner`/`dispatch_seam` and pin the legacy shell).

- [ ] **Step 1: Delete the three obsolete integration tests**

```bash
git rm crates/opex-core/tests/integration_fse_regression.rs \
       crates/opex-core/tests/integration_fse_affordance.rs \
       crates/opex-core/tests/integration_phase6_no_video_refs.rs
```

- [ ] **Step 2: Rewrite `integration_fse_security.rs` to the surviving surface**

Replace the whole file with (keeps the allowlist-safety coverage that still applies):

```rust
//! FSE allowlist security guards (surviving after legacy retirement).
//! `FSE_DEFAULT_ALLOWLIST` never contains dangerous tools, and the dispatch-time
//! `is_allowed_for_autorun` re-check fails closed. Both are shared with the
//! File Handler Hub's builtin gating.

use opex_core::agent::fse::allowlist::{is_allowed_for_autorun, FSE_DEFAULT_ALLOWLIST};

#[test]
fn allowlist_constant_excludes_code_exec_and_raw_fetch() {
    for forbidden in ["code_exec", "web_fetch", "workspace_write", "analyze_image"] {
        assert!(
            !FSE_DEFAULT_ALLOWLIST.contains(&forbidden),
            "{forbidden} must never be in the 0-click allowlist (FSE_DEFAULT_ALLOWLIST)"
        );
    }
    assert_eq!(
        FSE_DEFAULT_ALLOWLIST,
        &["transcribe", "describe", "extract_document", "save", "summarize_video"],
        "FSE_DEFAULT_ALLOWLIST must be exactly the five safe built-in actions"
    );
}

#[test]
fn is_allowed_for_autorun_rejects_code_exec_fail_closed() {
    let enabled: Vec<String> = FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect();
    assert!(!is_allowed_for_autorun("code_exec", &enabled));
    assert!(!is_allowed_for_autorun("transcribe", &[]));
    assert!(is_allowed_for_autorun("transcribe", &enabled));
}
```

- [ ] **Step 3: Compile**

Run: `make check`
Expected: clean (the rewritten test uses only surviving symbols; `is_allowed_for_autorun` + `FSE_DEFAULT_ALLOWLIST` still exist).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(fse): delete obsolete FSE integration tests, keep allowlist-safety coverage"
```

---

### Task C4: Remove the Telegram `fse:` callback

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` (FSE section ~158-275; test module ~578-616)
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` (intercept block ~120-124; guard test ~233-240)

**Interfaces:** removes the last non-test caller of `assert_fse_owner` and of `file_scenarios::run::run_scenario_and_persist`. `owner_gate.rs` still exists (deleted in C7); it is `pub`, so no dead-code failure in the interim.

- [ ] **Step 1: Delete the inline.rs FSE section + tests**

In `inline.rs`, delete the entire `// ── FSE callback ──` section: the header comment (~158), `parse_fse_callback` (~160-170), and `handle_fse_callback` (~172-275). Delete the `#[cfg(test)] mod fse_callback_tests` module (~578-616). Keep `use crate::agent::engine::AgentEngine;` (still used by clarify/approval handlers) and all clarify/approval/ping code.

- [ ] **Step 2: Remove the reader.rs intercept + guard test**

In `reader.rs`, delete the 5-line FSE intercept block (the `// FSE choice-callback intercept` comment + `let consumed_fse = inline::handle_fse_callback(...)` + `if consumed_fse { continue; }`, ~120-124) so the approval intercept flows straight into the clarify button-callback intercept. Delete the `fse_callback_wired_before_dispatch` test (~233-240). The other 3 `wire_guards` tests stay.

- [ ] **Step 3: Compile + run channel_ws tests**

Run: `make check` then `cargo test -p opex-core channel_ws -- --nocapture`
Expected: clean; surviving clarify/approval wire-guard tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/inline.rs \
        crates/opex-core/src/gateway/handlers/channel_ws/reader.rs
git commit -m "refactor(fse): remove Telegram fse: choice-callback + reader wiring"
```

---

### Task C5: Remove the `file_scenario` agent tool

**Files:**
- Delete: `crates/opex-core/src/agent/tool_handlers/file_scenario.rs`
- Modify: `crates/opex-core/src/agent/tool_handlers/mod.rs` (mod decl ~13; glob use ~28; register ~68; registry_tests ~74-93)
- Modify: `crates/opex-core/src/agent/pipeline/tool_defs.rs` (schema ~425-445; pin tests ~1310-1386)

**Interfaces:** removes the tool's calls to `crate::db::file_scenarios::*`, `crate::agent::fse::validate_binding_write`, and the `FILE_SCENARIO_CREATED` audit event.

- [ ] **Step 1: Delete the tool handler file**

```bash
git rm crates/opex-core/src/agent/tool_handlers/file_scenario.rs
```

- [ ] **Step 2: Fix `tool_handlers/mod.rs`**

Delete `mod file_scenario;` (~13), `use file_scenario::*;` (~28), the `r.register("file_scenario", FileScenarioHandler);` line (~68), and the whole `#[cfg(test)] mod registry_tests` module (~74-93, its only assertion is about `file_scenario` registration).

- [ ] **Step 3: Fix `pipeline/tool_defs.rs`**

Delete the `if ctx.is_base { tools.push(ToolDefinition { name: "file_scenario", ... }); }` block including its 5-line comment header (~425-445). Delete both pin tests `file_scenario_action_enum_is_create_list` (~1310-1363) and `file_scenario_absent_for_regular_agents` (~1365-1386). The `static_core_is_exactly_ten_tools` test (~1388+) is unrelated and stays (`file_scenario` was never in the static-core-10).

- [ ] **Step 4: Compile + run tool_defs tests**

Run: `make check` then `cargo test -p opex-core tool_defs -- --nocapture`
Expected: clean; `static_core_is_exactly_ten_tools` still passes.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/tool_handlers/mod.rs \
        crates/opex-core/src/agent/pipeline/tool_defs.rs
git commit -m "refactor(fse): remove the file_scenario agent tool + schema + pin tests"
```

---

### Task C6: Remove the `/api/file-scenarios/*` HTTP routes

**Files:**
- Delete: `crates/opex-core/src/gateway/handlers/file_scenarios/` (dir: `mod.rs`, `run.rs`)
- Modify: `crates/opex-core/src/gateway/handlers/mod.rs` (decl ~35)
- Modify: `crates/opex-core/src/gateway/mod.rs` (merge ~104)

**Interfaces:** removes `run_scenario_and_persist`, `api_run_scenario`, and the legacy allowlist handlers (their logic already lives in Task A1's `/api/handlers/allowlist`). `db/file_scenarios.rs` is NOT deleted here (still used by `dispatch_seam.rs` — deleted in C7).

- [ ] **Step 1: Delete the routes directory**

```bash
git rm -r crates/opex-core/src/gateway/handlers/file_scenarios
```

- [ ] **Step 2: Remove the module decl + merge**

In `crates/opex-core/src/gateway/handlers/mod.rs`, delete `pub(crate) mod file_scenarios;` (~35). In `crates/opex-core/src/gateway/mod.rs`, delete the line `.merge(handlers::file_scenarios::routes()) // /api/file-scenarios/* ...` (~104).

- [ ] **Step 3: Compile**

Run: `make check`
Expected: clean. (`run.rs` used `dispatch::dispatch_action` — `dispatch.rs` still exists until C7; `outcome` stays. `db::file_scenarios` still has `dispatch_seam.rs` as a caller until C7.)

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/mod.rs crates/opex-core/src/gateway/mod.rs
git commit -m "refactor(fse): remove /api/file-scenarios/* routes (allowlist moved to /api/handlers)"
```

---

### Task C7: Tear down the FSE module tree + seeder + dead code

**Files:**
- Delete: `crates/opex-core/src/agent/file_scenario/{dispatch.rs, dispatch_seam.rs, rewrite.rs, sniff.rs, owner_gate.rs}`
- Delete: `crates/opex-core/src/agent/fse/seeder.rs`
- Delete: `crates/opex-core/src/db/file_scenarios.rs`
- Modify (rewrite): `crates/opex-core/src/agent/file_scenario/mod.rs`
- Modify (rewrite): `crates/opex-core/src/agent/fse/mod.rs`
- Modify: `crates/opex-core/src/agent/fse/allowlist.rs` (delete `validate_binding_write` ~57-83; delete `AllowlistError::NotAllowlisted` variant ~22-25 + its Display arm ~34-38; delete 4 tests ~124-148)
- Modify: `crates/opex-core/src/lib.rs` (facade `pub mod file_scenario {...}` ~75-105; doc comment ~39-43)
- Modify: `crates/opex-core/src/agent/mod.rs` (comment on `pub mod fse;` ~44)
- Modify: `crates/opex-core/src/db/mod.rs` (decl ~30)
- Modify: `crates/opex-core/src/main.rs` (seeder call ~318-327)

**Interfaces:** after this task the only surviving FSE surface is `agent::file_scenario::outcome::{ScenarioOutcome, ScenarioStatus, FSE_DEFAULT_ALLOWLIST}` and `agent::fse::{allowlist, allowlist_store}` (the hub's allowlist surface). Everything else is gone.

- [ ] **Step 1: Delete the module files**

```bash
git rm crates/opex-core/src/agent/file_scenario/dispatch.rs \
       crates/opex-core/src/agent/file_scenario/dispatch_seam.rs \
       crates/opex-core/src/agent/file_scenario/rewrite.rs \
       crates/opex-core/src/agent/file_scenario/sniff.rs \
       crates/opex-core/src/agent/file_scenario/owner_gate.rs \
       crates/opex-core/src/agent/fse/seeder.rs \
       crates/opex-core/src/db/file_scenarios.rs
```

- [ ] **Step 2: Rewrite `agent/file_scenario/mod.rs`**

Replace the whole file with:

```rust
//! File Scenario Engine (FSE) — surviving outcome contract.
//!
//! After the legacy-FSE retirement (2026-07-01) only the toolgate wire type
//! (`ScenarioOutcome`/`ScenarioStatus`, parsed by `gateway/handlers/files.rs`)
//! and its `FSE_DEFAULT_ALLOWLIST` re-export remain here. Dispatch, the enrich
//! seam, the sniffer, the rewrite helper and the owner gate were removed with
//! the legacy post-send chips / Telegram `fse:` path.

pub mod outcome;
pub use outcome::{FSE_DEFAULT_ALLOWLIST, ScenarioOutcome, ScenarioStatus};
```

- [ ] **Step 3: Rewrite `agent/fse/mod.rs`**

Replace the whole file with:

```rust
//! File Scenario Engine (FSE) allowlist surface — surviving after legacy retirement.
//!
//! Keeps the hard-coded allowlist constant, the closed-domain toggle validator,
//! the dispatch-time autorun re-check, and the operator-editable toggle storage.
//! These are shared with the File Handler Hub (`match_buttons` / the new
//! `/api/handlers/allowlist` admin route). The seeder and the binding-write
//! validator went with the legacy `file_scenarios` bindings table.

pub mod allowlist;
pub mod allowlist_store;

// Flat public surface consumed by the hub's match_buttons + the handlers-admin route.
#[allow(unused_imports)]
pub use allowlist::{
    AllowlistError, FSE_DEFAULT_ALLOWLIST, is_allowed_for_autorun, validate_allowlist_toggle,
};
pub use allowlist_store::{get_enabled_allowlist, set_enabled_allowlist};

#[cfg(test)]
mod reexport_tests {
    use super::*;

    #[test]
    fn public_surface_is_re_exported_from_module_root() {
        assert_eq!(FSE_DEFAULT_ALLOWLIST.len(), 5);
        let full: Vec<String> = FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect();
        assert!(is_allowed_for_autorun("describe", &full));
        assert!(validate_allowlist_toggle(&["save".to_string()]).is_ok());
    }
}
```

- [ ] **Step 4: Remove `validate_binding_write` + `NotAllowlisted` in `fse/allowlist.rs`**

Delete `validate_binding_write` (doc + `#[allow(dead_code)]` + fn, ~57-83). Delete the `AllowlistError::NotAllowlisted(String)` variant + its doc (~22-25) and its `Display` match arm (~34-38). Delete the 4 tests that only cover `validate_binding_write`: `rejects_default_tool_outside_constant`, `accepts_default_tool_in_constant`, `rejects_member_disabled_in_toggle`, `ignores_skill_executor_and_non_default` (~124-148). Keep `is_constant_member` (still used by `is_allowed_for_autorun` + `validate_allowlist_toggle`) and the surviving tests (`constant_holds_exactly_the_five_builtins`, `autorun_recheck_is_fail_closed`, `toggle_rejects_non_constant_member`, `toggle_accepts_empty_slice`, `allowlist_contains_summarize_video`).

- [ ] **Step 5: Fix the `lib.rs` facade + doc**

In `crates/opex-core/src/lib.rs`, in the `pub mod file_scenario { ... }` facade block (~75-105), drop the `rewrite`, `dispatch`, and `owner_gate` `#[path=...] pub mod` mounts + the `pub use owner_gate::assert_fse_owner;` — keep only `outcome` + `pub use outcome::{ScenarioOutcome, ScenarioStatus};`. The `pub mod fse { allowlist }` block above it is unchanged. Result:

```rust
    pub mod fse {
        // Pure-data constants + validators. No crate::* deps.
        #[path = "allowlist.rs"]
        pub mod allowlist;
    }

    pub mod file_scenario {
        // Surviving toolgate wire type parsed by `gateway/handlers/files.rs`.
        #[path = "outcome.rs"]
        pub mod outcome;
        pub use outcome::{ScenarioOutcome, ScenarioStatus};
    }
```

Also edit the doc comment (~39-43) to drop the `dispatch`/`rewrite` + `integration_fse_regression.rs` mention (keep the `url_tools` line).

- [ ] **Step 6: Fix `agent/mod.rs` comment**

Edit the comment on `pub mod fse;` (~44) to drop the `dispatch/sniff/rewrite/seam` / "binding writes" mention, e.g.:

```rust
pub mod fse;  // allowlist constant + toggle validators + toggle storage (shared with File Handler Hub)
```

(`pub(crate) mod file_scenario;` at ~35 stays — the shrunk `outcome`-only module still exists.)

- [ ] **Step 7: Remove the db module decl + the seeder call**

In `crates/opex-core/src/db/mod.rs`, delete `pub mod file_scenarios;` (~30). In `crates/opex-core/src/main.rs`, delete the FSE seeder comment + the whole `match crate::agent::fse::seed_default_file_scenarios(&db_pool).await { ... }` block (~318-327).

- [ ] **Step 8: Compile + run fse/allowlist tests**

Run: `make check` then `cargo test -p opex-core fse:: -- --nocapture`
Expected: clean; `reexport_tests` + surviving allowlist tests pass. If `make check` flags a lingering reference to any deleted symbol, grep for it and remove that caller (all callers should already be gone from C1–C6).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(fse): tear down FSE module tree, seeder, and dead validators"
```

---

### Task C8: Deprecate migration + final gate

**Files:**
- Create: `migrations/069_fse_deprecate.sql`

**Interfaces:** none. Final task: adds the non-destructive deprecate migration and runs the whole-project gate + grep gate.

- [ ] **Step 1: Create the migration**

Create `migrations/069_fse_deprecate.sql`:

```sql
-- 069: Deprecate the legacy File Scenario Engine tables (File Handlers tab + FSE
-- retirement, 2026-07-01).
--
-- The legacy post-send "file-scenario chips" SSE affordance, the Telegram `fse:`
-- callback, the `file_scenario` agent tool, the `/api/file-scenarios/*` routes,
-- the in-core enrich sync-dispatch, and the startup seeder have all been removed.
-- The File Handler Hub (self-describing Python handlers in toolgate +
-- handler_jobs queue) supersedes them. Neither `file_scenarios` (m060) nor
-- `file_scenario_outcomes` (m061) is read or written by any surviving code path.
--
-- The tables and their rows are deliberately retained for audit/rollback safety;
-- this migration is purely documentary so the sequence stays monotonic.
-- Operators may remove the tables manually once retention is no longer needed.
--
-- The DO block is a no-op on fresh databases where these tables were never
-- created (migrations 060/061 create them; sqlx runs them before 069).
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = 'file_scenarios' AND n.nspname = 'public'
    ) THEN
        COMMENT ON TABLE file_scenarios IS
            'DEPRECATED (m069, 2026-07-01): superseded by the File Handler Hub (handler_jobs + toolgate handlers). No longer read/written.';
    END IF;

    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = 'file_scenario_outcomes' AND n.nspname = 'public'
    ) THEN
        COMMENT ON TABLE file_scenario_outcomes IS
            'DEPRECATED (m069, 2026-07-01): superseded by the File Handler Hub. No longer read/written.';
    END IF;
END $$;
```

- [ ] **Step 2: Run the migration + full DB suite**

Run: `make test-db`
Expected: migrations apply (069 is a documentary no-op that COMMENTs the two tables); full suite passes.

- [ ] **Step 3: Whole-project gate**

Run: `make check`, then `make lint` (`cargo clippy --all-targets -- -D warnings`), then `cd ui && npm test`, then `cd ui && npm run build`.
Expected: all clean. (Clippy is where a missed dead symbol — e.g. `NotAllowlisted` — would surface; if it fires, fix the flagged item.)

- [ ] **Step 4: Grep gate — zero residual references to deleted symbols**

Run each and expect ZERO hits (except this plan/spec docs):

```bash
grep -rn "file_scenario\b\|file-scenario\|FileScenario\|fileScenario" crates/ ui/src/ --include=*.rs --include=*.ts --include=*.tsx | grep -v "file_scenario/outcome\|file_scenario::outcome\|ScenarioOutcome\|ScenarioStatus\|// \|//!"
grep -rn "dispatch_attachments\|dispatch_seam\|rewrite_enriched_text\|assert_fse_owner\|validate_binding_write\|seed_default_file_scenarios\|FileScenarioChips\|build_file_scenario_chips\|run_scenario_and_persist" crates/ ui/src/
grep -rn "ScenarioChoice\|file-scenario-chips" ui/src/types/sse.generated.ts
grep -rn "NotAllowlisted" crates/
```

Any hit that is not `ScenarioOutcome`/`ScenarioStatus`/`FSE_DEFAULT_ALLOWLIST` (the MUST-STAY surface), a comment, or this plan/spec must be removed before the task is complete. Fix + re-run until clean.

- [ ] **Step 5: Commit**

```bash
git add migrations/069_fse_deprecate.sql
git commit -m "feat(db): m069 deprecate legacy file_scenarios + file_scenario_outcomes tables"
```

---

## Self-Review

**1. Spec coverage.** Every spec section maps to a task: Part A backend → A1; Part A frontend → A2; Part B page/nav/queries/types/i18n → B1; Part B chips UI → B2; Part C enrich → C1; chips wire + regen → C2; integration tests → C3; Telegram callback → C4; agent tool → C5; routes → C6; module shrink + seeder + dead code + owner_gate → C7; migration 069 + grep gate → C8. The "Verified facts" and "Risks" spec sections are folded into Global Constraints + task notes (empty-state, single-store, audit event, `gen-types`).

**2. Placeholder scan.** No TBD/TODO. Every code step shows the new code (creates/edits verbatim) or an exact file+line-range anchor for deletions. Deletion anchors quote enough context (identifier + line range) to be unambiguous.

**3. Type consistency.** `HandlerAdminRow` fields (Rust A1 ↔ TS A2) match by name; `match_`→`"match"` via serde rename ↔ TS `match`. `EnrichResult { text, video_accepted }` is consistent across C1 (subagent.rs, bootstrap.rs consumption). `FSE_DEFAULT_ALLOWLIST` = 5 members everywhere (A1 skeleton, C3 rewrite, C7 reexport test). `make gen-types` used consistently (C2, Global Constraints).

## Deploy notes

- Rust + migration `069` → `make remote-deploy` (syncs migrations). UI → local build + swap `~/opex/ui/out` (no make target for UI — build locally + tar/atomic-swap). No new toolgate code/deps.
- Post-deploy E2E: `/tools` → "File Handlers" tab lists the 5 builtins + any workspace handlers; toggling a builtin flips whether its button appears in the composer for a matching file; the old `/file-scenarios` route is gone; sending a file/message still works (enrich path intact); a video link still enqueues a `summarize_video` job (short-circuit ack).

## Out of scope / deferred

- The `*/*` mime-glob bug (the `save` builtin never matches) — a separate known follow-up.
- Untrusted-agent handler isolation; frame/vision in the video digest (hub follow-ups).
