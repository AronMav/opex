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

**Allowlist = ONE store (verified):** both `files.rs:239 → get_enabled_allowlist` (hub) and
the legacy `/api/file-scenarios/allowlist` handler write the same
`system_flags['fse.allowlist.enabled']` key via `agent/fse/allowlist_store.rs`
`get_enabled_allowlist` / `set_enabled_allowlist`. The allowlist gates **builtin-tier**
handlers only (the 5 in `FSE_DEFAULT_ALLOWLIST`); **workspace-tier** handlers are
always-on (not gated).

**MUST STAY (shared with the hub — do NOT delete):**
- `agent/fse/allowlist.rs` (`FSE_DEFAULT_ALLOWLIST`, `is_allowed_for_autorun`, `validate_allowlist_toggle`)
- `agent/fse/allowlist_store.rs` (`get_enabled_allowlist`, `set_enabled_allowlist`)
- `agent/fse/mod.rs` (re-exports of the allowlist surface)
- `agent/file_scenario/outcome.rs` (`ScenarioOutcome`, `ScenarioStatus` — the toolgate wire type `files.rs` parses)
- `agent/fse/owner_gate.rs` (`assert_fse_owner` — used by `files.rs`)
- `gateway/handlers/files.rs`, `agent/handler_registry.rs` (the hub itself)

**LEGACY-ONLY (delete in Phase C):**
- `agent/file_scenario/{dispatch.rs, dispatch_seam.rs, rewrite.rs, sniff.rs}`
- `gateway/handlers/file_scenarios/` (CRUD routes + `run.rs` `run_scenario_and_persist`)
- `db/file_scenarios.rs`
- `agent/tool_handlers/file_scenario.rs` (the `file_scenario` agent tool) + its registration + `tool_defs` schema
- post-send chips: `build_file_scenario_chips` (`sse_converter.rs`/`sse_writer.rs`), `opex_types::sse::ScenarioChoice`, the chips emission in `pipeline/bootstrap.rs`, the `"file-scenario-chips"` SSE event
- Telegram `fse:` callback: `channel_ws/inline.rs` `handle_fse_callback`/`parse_fse_callback` + the reader wiring
- `subagent.rs` `enrich_with_attachments`: remove the `dispatch_attachments` call → enrich becomes a pure text-annotation pass (no sync dispatch, no chips). NOTE: the video URL auto-trigger (`detect_video_links → insert_handler_job`) is a SEPARATE enrich branch (hub R13) — KEEP it.
- legacy tables `file_scenarios` (m060) and `file_scenario_outcomes` (m061) — non-destructive deprecate; deprecate `file_scenario_outcomes` ONLY after confirming the hub does not write to it.

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

- Delete `app/(authenticated)/file-scenarios/` (page + `ScenarioRow`/`ScenarioDialog`/`AllowlistEditor`).
- Remove the `nav.file_scenarios` sidebar entry from `components/app-sidebar.tsx`.
- Remove `useFileScenarios`/`useCreate/Update/Delete/SetDefault`/`useFileScenarioAllowlist`/
  `useSetFileScenarioAllowlist` from `queries.ts`; `FileScenario`/`FileScenarioAllowlistRow`
  from `types/api.ts`; `file_scenarios.*` + `nav.file_scenarios` i18n keys.
- Remove UI handling of the `file-scenario-chips` SSE event: drop it from the generated
  `types/sse.generated.ts` (regenerate from source) and any chat-side `ScenarioChoice`
  chip rendering. (Verify whether any live render exists; if only the type, clean the type.)

## Architecture — Part C: legacy FSE backend removal

Delete the LEGACY-ONLY set listed in the boundary section. Specifics:

- `subagent.rs` `enrich_with_attachments` → pure text-annotation pass; remove the
  `dispatch_attachments` call. KEEP the `detect_video_links → insert_handler_job` auto-trigger.
- Remove the chips wire: `build_file_scenario_chips` + `ScenarioChoice` +
  `"file-scenario-chips"` SSE + the `bootstrap.rs` emission.
- Remove the Telegram `fse:` callback (`inline.rs` `handle_fse_callback`/`parse_fse_callback` + reader wiring).
- Remove the `file_scenario` agent tool + registration + `tool_defs` schema.
- Remove `gateway/handlers/file_scenarios/` + `db/file_scenarios.rs` + `agent/file_scenario/{dispatch,dispatch_seam,rewrite,sniff}.rs`.
- KEEP `outcome.rs` + `owner_gate.rs` in `agent/file_scenario/` (shrunk module `mod.rs` keeps only these); relocating `outcome.rs` to its own module is optional (default: leave in place for a minimal diff).
- Migration `069_fse_deprecate.sql` (next free number): non-destructive `COMMENT ON TABLE`
  deprecation for `file_scenarios` (and `file_scenario_outcomes` ONLY if confirmed unwritten
  by the hub), guarded `IF EXISTS`, NO `DROP TABLE`.

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
  `clippy --all-targets -D warnings` clean; grep gate confirms no residual deleted symbols;
  integration tests compile.
- **UI (vitest):** the tab renders cards from a mocked `/api/handlers`; builtin toggle → PUT;
  workspace card shows "always on" + disabled toggle; localization. Removing file-scenarios
  does not regress the rest of the suite.

## Risks

- **`ScenarioChoice`/chips removed from `opex_types` + the generated SSE type** — ensure
  `sse.generated.ts` is regenerated from source so the type does not reappear.
- **`file_scenario_outcomes` (m061)** — deprecate ONLY after confirming the hub does not
  `INSERT` into it (grep). Default: deprecate `file_scenarios` only until confirmed.
- **enrich without `dispatch_attachments`** — the video URL auto-trigger
  (`detect_video_links → handler_jobs`) is a separate branch and MUST survive; verify E2E.

## Deploy notes

- Rust + migration `069` → `make remote-deploy` (syncs migrations). UI → local build + swap
  `~/opex/ui/out`. No new toolgate code/deps.
- Post-deploy: E2E — `/tools` → "File Handlers" tab lists 5 builtins + any workspace handlers;
  toggling a builtin changes whether its button appears in the composer.

## Out of scope / deferred

- The `*/*` mime-glob bug (the `save` builtin never matches — a separate known follow-up).
- Untrusted-agent handler isolation; frame/vision in the video digest (hub follow-ups).
