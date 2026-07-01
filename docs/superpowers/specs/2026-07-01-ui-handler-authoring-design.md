# UI file-handler authoring (edit / configure / create, incl. builtins)

**Date:** 2026-07-01
**Status:** Design approved, ready for implementation plan
**Follows:** [2026-07-01-file-handlers-tab-and-fse-retirement-design.md](2026-07-01-file-handlers-tab-and-fse-retirement-design.md) (the read-only File Handlers tab, shipped + deployed)

## Problem

The File Handlers tab currently lists handlers and toggles the builtin allowlist, but is
read-only — authoring a handler means hand-editing `.py` files in `/workspace`, and the 5
builtins cannot be edited at all (they ship in the toolgate source tree). The operator wants
to **edit, configure, and create** file handlers from the UI, **including the builtins**.

This reverses Decision #1 of the prior spec ("no inline editor — workspace authoring stays in
`/workspace`"). A handler is an executable Python file (an XML descriptor in a top comment
block + `async def run(ctx, file, params)`), so this adds an operator-facing code-authoring
surface.

## Decisions (locked during brainstorming)

| # | Decision | Choice |
|---|----------|--------|
| 1 | Edit depth | **Both** a code editor for the full `.py` AND a descriptor form (labels/icon/match/order/enabled). |
| 2 | Builtin editing | **Override → workspace.** Builtins stay read-only shipped defaults; editing a builtin seeds its source into `workspace/file_handlers/{id}.py`, which SHADOWS the builtin. Reversible via "reset to default" (delete the override). |
| 3 | Save safety | **Block-on-error.** toolgate validates the descriptor + Python (compile) + id rules before the file is written; errors surface inline in the UI and block the save; a successful save is live immediately via hot-reload. |
| 4 | Write path | **Core** writes `workspace/file_handlers/{id}.py` (never the builtin source); toolgate hot-reloads; core refreshes its manifest cache. |
| 5 | Authoring scope | **Operator-only via the UI**, behind bearer auth (same trust as `/workspace` editing + `code_exec`). Agents cannot author handlers — untrusted-agent isolation remains deferred. |
| 6 | Gating | By **id**: an id in `FSE_DEFAULT_ALLOWLIST` is allowlist-gated (`tier="builtin"`) whether pristine or overridden; a new id is `tier="workspace"` (always-on). |

## Grounded boundary (verified against the live tree, 2026-07-01)

- Handlers: `toolgate/handlers/` — `loader.py` (scan + hot-reload), `descriptor.py`
  (`parse_descriptor(source, tier) -> HandlerDescriptor`), `router.py` (`GET /handlers`,
  `GET /handlers/{id}`, `POST /handlers/{id}/run` — **no write endpoints**), `runner.py`,
  `context.py`, `builtin/*.py` (5: `describe`, `extract_document`, `save`, `summarize_video`,
  `transcribe`).
- `loader.py`: `load_all(builtin_dir, workspace_dir)` scans `builtin/` (tier `builtin`) then
  `{workspace_dir}/file_handlers` (tier `workspace`); `reload_file(path)` hot-reloads a
  workspace file on MODIFY; **builtin ids are reserved** — a workspace file reusing a builtin
  id is currently REJECTED; **builtins are never hot-reloaded**.
- `descriptor.py`: descriptor lives in a top comment block `# <handler> … # </handler>`
  (`_extract_block`); parses `id`, `labels`/`descriptions` (`lang=`), `icon`, `match` (`mime`
  ×N + `max_size_mb`), `capability`, `execution`, `output`, `params`, `order`, `enabled`;
  enforces `id ^[a-z0-9_-]+$`, ≥1 label, ≥1 mime.
- Core: `agent/handler_registry.rs` — `HandlerManifest` (id, labels, descriptions, icon,
  `match_`, capability, provider, execution, output, params, order, tier), `refresh()`
  (conditional GET of toolgate `/handlers`, fail-soft), `manifests()`.
- Core: `gateway/handlers/handlers_admin.rs` — `GET /api/handlers`, `GET/PUT
  /api/handlers/allowlist`, `HandlerAdminRow`. UI: `tools/page.tsx` `renderHandlerCard`;
  `queries.ts` `useHandlers`/`useHandlerAllowlist`/`useSetHandlerAllowlist`; `types/api.ts`
  `HandlerAdminRow`.
- Loopback bytes/URL, provenance, allowlist single-store, `match_buttons` gating — unchanged.

## Architecture — Part A: toolgate (override model + validation)

### A1. Loader: workspace shadows builtin (override)

`loader.py` changes so a `workspace/file_handlers/{id}.py` whose id matches a reserved builtin
id **shadows** the builtin (registers the workspace version as the effective handler) instead
of rejecting it. The pristine builtin remains available as the reset target (it is re-scanned
from `builtin/` and only shadowed when an override file exists). Manifest gains a **`source`**
field: `"builtin"` (pristine), `"override"` (builtin id shadowed by a workspace file),
`"workspace"` (non-builtin id). **`tier` is derived from the id**: reserved-builtin id →
`"builtin"` (pristine or override), else `"workspace"` — so allowlist gating (id-based) and the
UI toggle (tier-based) stay consistent. Overrides live in `file_handlers/` → already
hot-reloaded; builtins still never hot-reloaded (edits go to the override).

### A2. Validation endpoint

New `POST /handlers/validate` — body `{source, id?}` → runs `parse_descriptor` +
`py_compile`/`ast.parse` + the id rules **without registering** → `{ok: bool, descriptor?:
{...parsed fields...}, errors: [{field?, message}]}`. This is the block-on-error gate and also
lets the UI re-derive the descriptor form from edited code (the parsed `descriptor` in the
response repopulates the form).

## Architecture — Part B: core (admin endpoints)

New handlers in `gateway/handlers/handlers_admin.rs` (operator-only, behind the existing global
bearer auth; not loopback-exempt):

- `GET /api/handlers/{id}/source` → `{id, source, source_kind}` where `source_kind` ∈
  `builtin|override|workspace` (matches the manifest `source` field) and `source` is the raw `.py`:
  the override if present, else the builtin source (starting point for a new override), else the
  workspace file. Core reads the builtin source directly from the runtime toolgate dir
  (`~/opex/toolgate/handlers/builtin/{id}.py`) — read-only.
- `POST /api/handlers` (create) → `{id, source}` → validate via toolgate `/handlers/validate` →
  on `ok` write `workspace/file_handlers/{id}.py` → refresh cache → 201; on invalid → 4xx with
  `errors`. Rejects an id that already exists (workspace or builtin) — creating over a builtin
  is an edit (PUT), not a create.
- `PUT /api/handlers/{id}` (edit) → `{source}` → validate → write. For a builtin id with no
  override → creates the override file (seed+edit); for a workspace id (or existing override) →
  overwrites. → refresh cache.
- `DELETE /api/handlers/{id}` → workspace id → delete the file; builtin override → delete the
  override file (**reset to default** — the pristine builtin resurfaces); pristine builtin →
  400 (shipped builtins are not deletable). → refresh cache.
- Existing `GET /api/handlers` list rows gain `source` (from A1). `GET/PUT
  /api/handlers/allowlist` unchanged.

**Write safety:** `id` re-validated against `^[a-z0-9_-]+$` (no path separators → the path is
always `workspace/file_handlers/{id}.py`, no traversal); writes go ONLY into
`workspace/file_handlers/` (never the builtin source); a file-size cap; every
create/edit/delete/reset emits an audit event (mirrors `FSE_ALLOWLIST_AMENDED`).

**Form ↔ source (single source of truth = the `.py`):** the descriptor form is populated from
the already-fetched manifest row (parsed fields) for reads; on a form change the UI regenerates
the `# <handler> … </handler>` comment block within the source string (a small, tested
client-side render) and the whole source is what gets PUT. After a raw-code edit, calling
`/handlers/validate` returns the parsed `descriptor` to refresh the form. Core never parses the
descriptor — toolgate is the authority.

## Architecture — Part C: UI (extend the File Handlers tab)

- Each card gains **Edit** and **Delete/Reset** actions (+ the existing builtin allowlist
  toggle). A status badge: `default` (pristine builtin) / `edited (override)` / `workspace`.
- A **"Create handler"** button → dialog: `id` + a starter template (or "duplicate" an existing
  handler as the starting point).
- **Editor** (Edit/Create), mirroring the `/workspace` + YAML-tools editor pattern:
  **CodeMirror (Python)** for the full `.py` + a **descriptor form** (labels ru/en, icon, mime
  globs, `max_size_mb`, execution, order, enabled) that stays in sync with the source. **Save**
  → validate → block-on-error (inline red errors on the offending descriptor/Python) → on
  success hot-reload → live; list invalidated.
- **Reset to default** on an overridden builtin (deletes the override); **Delete** on a
  workspace handler. A link "Advanced editing in /workspace" (workspace handlers live there).
- `queries.ts`: `useHandlerSource`, `useCreateHandler`, `useUpdateHandler`, `useDeleteHandler`.
  `types/api.ts`: `HandlerSourceDto` + `HandlerAdminRow.source`. i18n: `tools.handler_edit/
  create/delete/reset/…` in `locales/{en,ru}.json`.

## Security

- Operator-only, behind the existing bearer auth (all `/api/*`). Same trust as `/workspace`
  editing + `code_exec` — this is a UI for writing executable Python the operator runs
  ("trusted author v1").
- Agents **cannot** author handlers (no agent tool). Untrusted-agent handler isolation remains
  a deferred follow-up; this feature does not open it.
- Writes strictly into `workspace/file_handlers/{id}.py`; `id ^[a-z0-9_-]+$` (traversal-safe);
  builtin source read-only; file-size cap.
- Block-on-error validation prevents shipping broken/half code silently.
- Audit event on create/edit/delete/reset.

## Testing (TDD)

- **toolgate (pytest):** loader override/shadow (workspace shadows builtin; reset → builtin
  resurfaces; override id vs new id); `POST /handlers/validate` (valid/invalid descriptor,
  invalid Python, id rules) without registering; `source`/`tier` in the manifest for
  builtin/override/workspace.
- **core (cargo):** `GET /{id}/source` (override→builtin→workspace precedence);
  create/edit/delete/reset (writes to workspace, builtin→override seeding, reset-to-default,
  collision rules, id/traversal guard, validate-before-write → 4xx-with-no-write, refresh, audit
  event).
- **UI (vitest):** editor renders code + form; form ↔ source sync (descriptor-block render);
  save blocked on validation error (inline red); create/delete/reset flows; per-card actions;
  status badges.
- **E2E (post-deploy, server):** create a workspace handler from the UI → it appears in a
  matching file's `/actions`; edit a builtin → override active + edit visible; reset → default;
  invalid save → 4xx + not written.

## Risks

- **Loader shadow change** must keep the pristine builtin available for reset (re-scan builtin/;
  only shadow when an override file exists) and must not break the id-reservation invariant for
  NON-override collisions (two workspace files, same non-builtin id → still an error).
- **Descriptor-block render (client-side)** must round-trip: render → toolgate parse must yield
  the same fields. Covered by validate-returns-descriptor + vitest.
- **Deploy:** toolgate `.py` changes (loader + validate) need the toolgate sync + core restart
  re-spawn (server-deploy syncs toolgate subpackages); UI needs the manual `ui/out` swap;
  builtins remain in the source tree (overrides in workspace survive deploy).

## Out of scope / deferred (YAGNI)

- Param-schema editing via the form (v1: descriptor fields only; param schema editable in code).
- Handler versioning / diff / history.
- Agent-authored handlers (untrusted isolation still deferred).
- Sandboxed test-run before save (Decision #3 chose block-on-error, not test-run).
- The `*/*` mime-glob `save` bug (separate known follow-up).
