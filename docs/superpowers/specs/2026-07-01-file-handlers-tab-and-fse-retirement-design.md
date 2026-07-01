# File Handlers Tools tab + legacy FSE retirement

**Date:** 2026-07-01
**Status:** Design approved, ready for implementation plan
**Follows:** [2026-06-30-file-handler-hub-design.md](2026-06-30-file-handler-hub-design.md) (the File Handler Hub, now shipped)

## Problem

The File Handler Hub shipped: per-file action buttons in the chat composer, backed by
self-describing Python handlers in toolgate. Two UI gaps remain:

1. **No management surface** for the handlers. They are only reachable per-file via
   `GET /api/files/{upload_id}/actions` (the composer). There is no page to see the full
   set of handlers or toggle which builtins are offered.
2. **The old "Сценарии файлов" (File Scenarios) tab is now obsolete.** It manages the
   legacy FSE `file_scenarios` bindings (the pre-hub post-send SSE chips + Telegram `fse:`
   callbacks), which the hub supersedes. The user wants the new handlers surfaced as a
   separate tab under the "Инструменты" (Tools) menu and the legacy tab removed.

## Decisions (locked during brainstorming)

| # | Decision | Choice |
|---|----------|--------|
| 1 | New tab scope | **List + enable/disable toggle** (the toggle drives the shared allowlist that gates which builtin handlers appear). No inline `.py` editor — workspace authoring stays in `/workspace`. |
| 2 | Fate of legacy "Сценарии файлов" | **Remove fully — UI + backend** (page, routes, `file_scenarios` table, post-send chips, Telegram `fse:` callback, the `file_scenario` agent tool, the in-core sync dispatch), keeping the parts shared with the hub. |
| 3 | Allowlist storage | **Single store** — `system_flags['fse.allowlist.enabled']`, read by BOTH the hub's `match_buttons` (via `get_enabled_allowlist`) AND the old `/api/file-scenarios/allowlist` endpoint. The new tab's toggle reuses `get/set_enabled_allowlist`; it genuinely gates the builtin handlers. |
| 4 | Table disposition | **Non-destructive** deprecate migration (comment only, no `DROP TABLE`), like `068`. |

## Grounded boundary: legacy-only vs shared

> All file:line references below were verified against the live tree during the spec
> review (2026-07-01). See §"Verified facts" for the confirmations that shaped this list.

**Allowlist = ONE store (verified):** both `files.rs:239,276 → get_enabled_allowlist` (hub)
and the legacy PUT handler (`file_scenarios/mod.rs:466,476 → set_enabled_allowlist`) read/write
the same `system_flags['fse.allowlist.enabled']` key
(`agent/fse/allowlist_store.rs:12 ALLOWLIST_FLAG_KEY`). The allowlist gates **builtin-tier**
handlers only (the 5 in `FSE_DEFAULT_ALLOWLIST`); **workspace-tier** handlers are
always-on (not gated).

**MUST STAY (shared with the hub — do NOT delete):**
- `agent/fse/allowlist.rs` (`FSE_DEFAULT_ALLOWLIST`, `is_allowed_for_autorun`,
  `validate_allowlist_toggle`). **NOTE:** `validate_binding_write` (allowlist.rs:67) has
  no non-test caller after Phase C (only the deleted `file_scenario` tool + routes call it) →
  it becomes dead code that trips `clippy -D warnings`. Phase C must ALSO drop
  `validate_binding_write` + its re-export in `fse/mod.rs`.
- `agent/fse/allowlist_store.rs` (`get_enabled_allowlist`, `set_enabled_allowlist`)
- `agent/fse/mod.rs` (re-exports of the SURVIVING allowlist surface only — drop the
  `seed_default_file_scenarios` and `validate_binding_write` re-exports; see LEGACY-ONLY)
- `agent/file_scenario/outcome.rs` (`ScenarioOutcome`, `ScenarioStatus` — the toolgate wire
  type `files.rs` parses; it also `pub use`s `FSE_DEFAULT_ALLOWLIST` from `fse::allowlist`,
  keep that re-export line)
- `gateway/handlers/files.rs`, `agent/handler_registry.rs` (the hub itself)

**LEGACY-ONLY (delete in Phase C):**
- `agent/file_scenario/{dispatch.rs, dispatch_seam.rs, rewrite.rs, sniff.rs, owner_gate.rs}`
  — `owner_gate.rs`'s `assert_fse_owner` (file_scenario/owner_gate.rs:23) is called by
  exactly ONE non-test site, the Telegram `fse:` callback at `channel_ws/inline.rs:206`
  (deleted below); once that goes, `assert_fse_owner` is dead → clippy fails. Delete it, its
  `lib.rs:104` facade re-export, and its `file_scenario/mod.rs:33` re-export. (The spec
  previously mis-listed this in MUST-STAY as "used by files.rs" — `files.rs` uses
  `assert_upload_accessible`; the "owner_gate" string in `files.rs:883` is only a test name.)
- `agent/fse/seeder.rs` (`seed_default_file_scenarios`) + its call at `main.rs:323` + the
  `pub mod seeder;` / `pub use seeder::…` in `fse/mod.rs:12,21`. It raw-`INSERT`s default rows
  into `file_scenarios`; after Phase C the only reader of that table (`dispatch_seam`) is gone,
  so the seeder is live dead-work writing a retired table.
- `gateway/handlers/file_scenarios/` (CRUD routes + `run.rs` `run_scenario_and_persist` + the
  legacy `/api/file-scenarios/allowlist` GET/PUT — its logic MOVES to `/api/handlers/allowlist`)
- `db/file_scenarios.rs` (only writer of `file_scenario_outcomes`, via `insert_outcome`)
- `agent/tool_handlers/file_scenario.rs` (the `file_scenario` agent tool) + its registration in
  `tool_handlers/mod.rs:13,28,68` (+ test :79-86) + its schema in `tool_defs.rs:432`
  (+ pin tests :1310-1384)
- **Enrich sync-dispatch** in `agent/pipeline/subagent.rs::enrich_message_text` (NOT
  `enrich_with_attachments`): remove the `dispatch_attachments` call (subagent.rs:268-279) AND
  the adjacent `rewrite::rewrite_enriched_text` call (subagent.rs:280-284). KEEP
  `url_tools::enrich_with_attachments` (url_tools.rs:33 — already a pure text-annotation helper,
  never called dispatch). KEEP the SEPARATE video URL auto-trigger
  (`detect_video_links → insert_handler_job`, subagent.rs:242-262, hub R13).
- **Chips dataflow** (removing dispatch orphans these — all must go together):
  `EnrichResult.pending_alternatives` + `EnrichResult.outcomes` (subagent.rs:186-198);
  `BootstrapOutcome.pending_alternatives` (bootstrap.rs:69); the `AffordanceTransport` enum +
  `affordance_transport()` helper (bootstrap.rs:21-33) + the emission block (bootstrap.rs:382-450)
  + their tests (bootstrap.rs:545-552, 576-587); the destructure sites in `engine/run.rs`
  (4 pairs at ~96/125, 371/391, 528/548, 724/768) + `execute.rs:124`. `video_accepted` then
  reduces to the URL-enqueue flag alone (drop the `|| outcomes.iter().any(...)` at bootstrap.rs:288).
- **Chips wire** (Rust): `StreamEvent::FileScenarioChips` (`agent/stream_event.rs:78-82`) + its
  test (:142-158); the coalescer arm (`gateway/sse/coalescer.rs:37`);
  `build_file_scenario_chips` (`sse_writer.rs:86,297-303`) + test (:685-696) and its call in
  `sse_converter.rs:468-474` + guard test (:586-588); `opex_types::sse::ScenarioChoice`
  (`opex-types/src/sse.rs:295`) + `SseEvent::FileScenarioChips` (sse.rs:120-128); the codegen
  registration `register_ts_dto!(ScenarioChoice,…)` + import in `dto_export/sse_ts.rs:16,29`
  (**edit this BEFORE regenerating** `sse.generated.ts` — regen is a no-op otherwise).
- Telegram `fse:` callback: `channel_ws/inline.rs` `handle_fse_callback`/`parse_fse_callback`
  (inline.rs:160-244) + its tests (:580-593) + the reader wiring (`channel_ws/reader.rs:121`)
  + the reader guard test (`reader.rs:236`).
- **Rust integration tests** that reference deleted symbols: delete
  `tests/{integration_fse_regression.rs, integration_fse_affordance.rs, integration_phase6_no_video_refs.rs}`
  entirely; rewrite `tests/integration_fse_security.rs` to drop the `dispatch` import while
  keeping any `fse::allowlist` coverage (or fold that into the new handlers-admin tests).
- legacy tables `file_scenarios` (m060) and `file_scenario_outcomes` (m061) — non-destructive
  deprecate. `file_scenario_outcomes` IS safe to deprecate: the hub never writes it (only the
  deleted `db/file_scenarios.rs` + `file_scenarios/run.rs` do — verified).

## Architecture — Part A: new "File Handlers" tab

### Backend

New handler module `crates/opex-core/src/gateway/handlers/handlers_admin.rs`
(`pub(crate) fn routes() -> Router<AppState>`, merged in `gateway/mod.rs`, behind auth):

- `GET /api/handlers` → `{ handlers: [HandlerAdminRow...] }`. Each row = a manifest from
  `state.handlers.refresh()` + `manifests()`, enriched with `enabled`:

  ```json
  { "id":"transcribe", "labels":{"ru":"…","en":"…"}, "descriptions":{"ru":"…","en":"…"},
    "icon":"mic", "match":{"mime":["audio/*","video/*"],"max_size_mb":200},
    "capability":"stt", "provider":"OpenAI Whisper", "execution":"sync",
    "output":"text", "order":10, "tier":"builtin", "enabled":true }
  ```

  `enabled`: for `tier=="builtin"` → `is_allowed_for_autorun(id, get_enabled_allowlist(db))`;
  for `tier=="workspace"` → always `true` (workspace handlers are not allowlist-gated).

- `GET /api/handlers/allowlist` → `{ allowlist: [{action_ref, enabled}] }` — the 5
  `FSE_DEFAULT_ALLOWLIST` members + their enabled state (wrapper over `get_enabled_allowlist`).
- `PUT /api/handlers/allowlist` body `{action_ref, enabled}` → `set_enabled_allowlist`
  (validated by `validate_allowlist_toggle` — only const members). This is a clean new
  route over the SAME store the old `/api/file-scenarios/allowlist` used (which is removed
  in Phase C).

### Frontend

Third tab in `ui/src/app/(authenticated)/tools/page.tsx` (shadcn `Tabs`, alongside
"External APIs" + "MCP Servers"): `TabsTrigger value="handlers"`.

- Card grid (mirrors the YAML/MCP card pattern in `ToolHelpers.tsx`). Each card: icon +
  localized label, tier badge (builtin/workspace), execution (sync/async), mime globs,
  capability→provider (if present), description. Toggle:
  - builtin → `Switch` → `PUT /api/handlers/allowlist {action_ref: id, enabled}` → invalidate.
  - workspace → "always on" badge, toggle disabled (allowlist does not gate them).
- A "Add your own handler" link → `/workspace` (author `workspace/file_handlers/*.py`;
  no inline editor in v1).
- `queries.ts`: `useHandlers()` (`GET /api/handlers`), `useHandlerAllowlist()`,
  `useSetHandlerAllowlist()`. `types/api.ts`: `HandlerAdminRow`, `HandlerAllowlistRow`.
- i18n: `tools.file_handlers` = "File Handlers"/"Обработчики файлов" + sub-strings
  (builtin/workspace/sync/async/provider/"always on") in `locales/{en,ru}.json`.

`GET /api/handlers` lists ALL registered handlers (no upload needed), unlike
`/api/files/{id}/actions` which matches for one concrete file. Read-only + toggle; no CRUD
(handlers are files/builtins, not DB rows).

## Architecture — Part B: legacy FSE frontend removal

- Delete `app/(authenticated)/file-scenarios/` (page + `ScenarioRow`/`ScenarioDialog`/
  `AllowlistEditor` + the `__tests__/` dir under it).
- Remove the `nav.file_scenarios` sidebar entry from `components/app-sidebar.tsx:77` AND the
  now-unused `FileCog` icon import (`app-sidebar.tsx:29`) — ESLint `no-unused-vars` fails
  `npm run build` otherwise.
- Remove `useFileScenarios`/`useCreate/Update/Delete/SetDefault`/`useFileScenarioAllowlist`/
  `useSetFileScenarioAllowlist` from `queries.ts` (+ `lib/__tests__/file-scenarios-queries.test.ts`);
  `FileScenario`/`FileScenarioAllowlistRow` AND the hand-written `ScenarioChoice` interface
  (`types/api.ts:516`) from `types/api.ts`; `file_scenarios.*` + `nav.file_scenarios` i18n keys.
- **`file-scenario-chips` is LIVE UI code, not just a type** — remove all of it:
  - the `case "file-scenario-chips"` handler in `stores/stream/stream-processor.ts:356-366`
    (+ the `FileScenarioChipsPart` import at :25);
  - the `FileScenarioChipsPart` interface + its `MessagePart` union member in
    `stores/chat-types.ts:92-108`;
  - the SSE fixture `__tests__/fixtures/sse/file-scenario-chips.json` + the case that loads it
    in `__tests__/sse-events.fixtures.test.ts:178-226`;
  - the guard test `stores/stream/__tests__/fse-chips.test.ts` (asserts the case is wired → will
    fail once removed).
  - `__tests__/sse-fse-codegen.test.ts` — currently asserts `sse.generated.ts` CONTAINS
    `"file-scenario-chips"` + `ScenarioChoice`; delete it (or invert), else it fails after regen.
- Regenerate `types/sse.generated.ts` via `make gen-ts` (see Part C — the Rust codegen source
  must be edited first, or the regen re-emits the removed type).

## Architecture — Part C: legacy FSE backend removal

Delete the LEGACY-ONLY set enumerated in the boundary section (every symbol there has a
file:line). Recommended sequencing to keep intermediate `cargo check` green:

1. **Enrich first** (`subagent.rs::enrich_message_text`): drop the `dispatch_attachments` +
   `rewrite_enriched_text` calls (subagent.rs:268-284), then drop
   `EnrichResult.{pending_alternatives,outcomes}` and rewire `video_accepted` to the URL-enqueue
   flag. This unblocks deleting `dispatch_seam.rs`/`rewrite.rs`.
2. **Chips dataflow + wire**: remove `BootstrapOutcome.pending_alternatives`, the
   `AffordanceTransport`/`affordance_transport()` emission (bootstrap.rs:21-33, 382-450) and its
   `engine/run.rs`/`execute.rs` destructures; then the wire —
   `StreamEvent::FileScenarioChips`, coalescer arm, `build_file_scenario_chips`,
   `opex_types::sse::ScenarioChoice` + `SseEvent::FileScenarioChips`, and the
   `dto_export/sse_ts.rs` registration.
3. **Regenerate TS types**: `make gen-ts` (`cargo run --features ts-gen --bin gen_ts_types -p
   opex-core`, Makefile) — only now does `sse.generated.ts` lose the chips type.
4. **Telegram callback**: `inline.rs` `handle_fse_callback`/`parse_fse_callback` + `reader.rs`
   wiring — this removes the last `assert_fse_owner` caller, so now delete
   `owner_gate.rs` + its `lib.rs`/`file_scenario/mod.rs` re-exports.
5. **Tool + routes + db + seeder**: `file_scenario` agent tool (+ `tool_handlers/mod.rs`,
   `tool_defs.rs`), `gateway/handlers/file_scenarios/`, `db/file_scenarios.rs`,
   `agent/file_scenario/{dispatch,dispatch_seam,rewrite,sniff}.rs`, `agent/fse/seeder.rs` +
   `main.rs:323` call, and `validate_binding_write` (allowlist.rs) which is now dead.
6. **Tests**: delete/rewrite the Rust integration tests + in-crate guard tests named in the
   boundary (they assert the removed symbols are wired and will actively fail).

Shrunk `agent/file_scenario/mod.rs` keeps ONLY:
`pub mod outcome;` + `pub use outcome::{FSE_DEFAULT_ALLOWLIST, ScenarioOutcome, ScenarioStatus};`
(drop the `dispatch`/`dispatch_seam`/`rewrite`/`sniff`/`owner_gate` `pub mod`+`pub use` pairs).

- Migration `069_fse_deprecate.sql` (next free number — highest existing is `068`): non-destructive
  `COMMENT ON TABLE` deprecation for BOTH `file_scenarios` (m060) and `file_scenario_outcomes`
  (m061) — the hub writes neither (verified) — guarded `IF EXISTS`, NO `DROP TABLE`, mirroring the
  `068` pattern.
- **Gate:** per-step `cargo check`; at the end `clippy --all-targets -D warnings` +
  `cargo test --all-targets` (compiles the integration tests) + a grep gate confirming no
  residual references to the deleted symbols.

## Phasing

- **A — New tab** (`/api/handlers` + `/api/handlers/allowlist` + the Tools tab). Ships
  standalone alongside the still-present old page. Allowlist now controllable from the new tab.
- **B — Legacy frontend removal** (page, nav, queries, types, i18n, chips UI). Old page gone;
  allowlist already lives in the new tab.
- **C — Legacy backend removal** (dispatch/chips/Telegram/routes/table/tool). Call-sites first,
  then definitions. Per-task gate `cargo check`; full `clippy --all-targets -D warnings` + a
  grep gate (no references to deleted symbols) at the end.

Between A and B the allowlist is editable from two places (brief, acceptable); after B, only
the new tab.

## Testing (TDD)

- **Backend (cargo):** `GET /api/handlers` returns manifests + correct `enabled` (builtin per
  allowlist, workspace always-true); `PUT /api/handlers/allowlist` mutates
  `system_flags['fse.allowlist.enabled']` (const validation; invalid → 4xx) and thereby changes
  `match_buttons` output (same store — assert the link). Post-C: `cargo check --all-targets` +
  `clippy --all-targets -D warnings` clean; `cargo test --all-targets` compiles (the four FSE
  integration tests are deleted/rewritten per Part C, not left dangling); grep gate confirms no
  residual deleted symbols. The in-crate guard tests that pin the removed symbols
  (`reader.rs:236`, `sse_converter.rs:586-588`, `bootstrap.rs:576-587`, `tool_defs.rs:1310-1384`,
  `inline.rs:580-593`, `stream_event.rs:142-158`) are removed as part of their symbol's deletion.
- **UI (vitest):** the tab renders cards from a mocked `/api/handlers`; builtin toggle → PUT;
  workspace card shows "always on" + disabled toggle; localization. The chips/file-scenarios
  guard tests are deleted (not expected to pass) — see Part B. Removing file-scenarios does not
  regress the rest of the suite.

## Verified facts (from the 2026-07-01 grounded spec review)

- **Single allowlist store — TRUE.** `allowlist_store.rs:12 ALLOWLIST_FLAG_KEY =
  "fse.allowlist.enabled"`; read by the hub (`files.rs:239,276`), written by the legacy PUT
  (`file_scenarios/mod.rs:466,476`). The new `/api/handlers/allowlist` reuses the same store.
- **`file_scenario_outcomes` (m061) unwritten by the hub — TRUE.** Only writers are the deleted
  `db/file_scenarios.rs` (`insert_outcome`) + `file_scenarios/run.rs` → safe to deprecate.
- **Migration `069` is the next free number** (highest existing `068`).
- **`assert_fse_owner` is legacy-only** — the only non-test caller is `inline.rs:206` (deleted);
  `files.rs` does NOT use it. Reclassified to LEGACY-ONLY.
- **`enrich_with_attachments` ≠ the dispatch site** — the dispatch call is in
  `enrich_message_text` (subagent.rs:268); `enrich_with_attachments` (url_tools.rs:33) is already
  a pure text helper and stays.
- **`PendingAlternative`/`ScenarioChoice` cause no dangling type** — both are defined in
  `dispatch_seam.rs` and consumed only by the `EnrichResult`/`BootstrapOutcome` fields removed in
  the same phase; `opex_types::sse::ScenarioChoice` is a separate wire type with no resume/replay
  reference.

## Risks

- **`ScenarioChoice`/chips removed from `opex_types` + the generated SSE type** — the codegen
  SOURCE (`dto_export/sse_ts.rs` + `opex-types/src/sse.rs`) must be edited BEFORE `make gen-ts`,
  else the regen re-emits the type. See Part C sequencing.
- **enrich without `dispatch_attachments`** — the video URL auto-trigger
  (`detect_video_links → handler_jobs`, subagent.rs:242-262) is a separate branch and MUST
  survive; verify E2E. Removing dispatch drops the `outcomes`-based `video_accepted` term, so
  confirm the URL-enqueue path still sets `video_accepted` correctly.
- **First-boot empty handler list** — `state.handlers.refresh()` is fail-soft (keeps stale/empty
  cache on toolgate error, returns `()` not `Result`); if toolgate never responded, `/api/handlers`
  returns zero handlers. The new tab needs an empty-state hint (not an error).

## Deploy notes

- Rust + migration `069` → `make remote-deploy` (syncs migrations). UI → local build + swap
  `~/opex/ui/out`. No new toolgate code/deps.
- Post-deploy: E2E — `/tools` → "File Handlers" tab lists 5 builtins + any workspace handlers;
  toggling a builtin changes whether its button appears in the composer.

## Out of scope / deferred

- The `*/*` mime-glob bug (the `save` builtin never matches — a separate known follow-up).
- Untrusted-agent handler isolation; frame/vision in the video digest (hub follow-ups).
