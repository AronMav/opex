# Hermes-parity Phase 1 — design

- **Date:** 2026-06-20
- **Status:** Approved (design); pending implementation plan
- **Branch:** `feat/hermes-parity-phase1`
- **Origin:** Gap analysis of OPEX vs current Hermes (`D:/GIT/hermes-agent` @ `b88d0007c`). See memory `reference_hermes_agent.md`.

## Context & motivation

A workflow-driven comparison of OPEX against the latest Hermes surfaced a ranked
list of missing functionality. This spec covers **Phase 1** — the four items that
deliver real value *and* are locally verifiable with `cargo test` / `pytest`:

| # | Component | Value | Effort |
|---|-----------|-------|--------|
| A | Email channel allowlist fix | bug fix | S |
| B | Browser actions: scroll/hover/drag/back/press + dialog | high | M |
| C | Session-scoped `todo` tool | medium | S/M |
| D | Prompt-injection: warn → block for identity files | medium | M |

**Out of scope (Phase 2, separate spec):** `/goal` autonomous loop (E), `/voice`
per-chat toggle (F), `/compact` in UI SlashMenu (G). These touch the live pipeline /
channels stack and cannot be fully verified locally; each warrants its own design.

## Cross-cutting principles

- **TDD**: tests first, then implementation, for every component (project convention).
- **Verification reality**: local machine has Rust 1.95, Node 22, Docker — but **no Bun**
  and no running Postgres/toolgate stack. So: Rust pure logic + UI logic verified locally
  (`cargo test`, `vitest`); DB-backed tests via `make test-db`; full browser/integration
  verified on the server after `make remote-deploy`. The spec marks each component's
  local-verifiability explicitly.
- **Isolation**: each component is an independently understandable unit with a clear
  interface; no unrelated refactoring.

---

## Component A — Email channel allowlist fix

### Problem
`channels/src/drivers/email.ts` exists and is registered in the adapter switch, but
`POST /api/agents/{name}/channels` with `channel_type="email"` returns **400** because
`"email"` is absent from the `SUPPORTED` allowlist in
`crates/opex-core/src/gateway/handlers/channels.rs:122`. Credentials handling already
covers email: the secret is the `password` field, which `extract_credentials` already
captures into the vault; `imap_host`/`smtp_host`/`imap_user` are non-secret config.

### Design
1. Promote the inline `const SUPPORTED` (currently inside `api_channel_create`) to a
   module-level `const SUPPORTED_CHANNEL_TYPES: &[&str]` so it is testable and reusable.
2. Add `"email"` to it.

### Files
- `crates/opex-core/src/gateway/handlers/channels.rs`

### Tests
- `cargo test`: assert `SUPPORTED_CHANNEL_TYPES` contains `"email"` (and the existing six).

### Local-verifiable: ✅

---

## Component B — Browser actions: scroll/hover/drag/back/press + dialog

### Problem
`browser_action` covers ~30% of a usable browser surface. Missing `scroll` breaks most
real web automation (infinite scroll, dropdowns, hover menus). No JS-dialog handling can
hang automation. The Rust handler `handle_browser_action`
(`agent/pipeline/handlers.rs`) is a **thin pass-through** — it forwards the args JSON to
the browser-renderer `/automation` endpoint — so new actions are implemented in Python
plus the tool schema; the Rust handler is unchanged.

### Design
**Python — `docker/browser-renderer/app.py`:**
- Refactor the monolithic `automation()` body into `async def dispatch_action(page, req)`
  so the action routing is unit-testable with a fake `Page` (no real browser). The FastAPI
  `automation()` becomes a thin wrapper (session resolution + `dispatch_action`).
- New actions:
  - `scroll` — `selector` → `element.scroll_into_view_if_needed()`; else `dy` pixels →
    `window.scrollBy`; else `to:"bottom"|"top"` → scroll to page extreme.
  - `hover` — `page.hover(selector)`.
  - `drag` — `page.drag_and_drop(selector, to_selector)`.
  - `back` — `page.go_back()`.
  - `press` — `page.press(selector, key)` if `selector` else `page.keyboard.press(key)`.
  - `set_dialog` — set per-session dialog behaviour (`accept`/dismiss, optional
    `prompt_text`).
- Dialog handling: at `create_session`, register `page.on("dialog", handler)` that, by
  default, **accepts** dialogs and records the last dialog message in a per-session dict;
  `set_dialog` toggles accept/dismiss and prompt text. This removes the hang risk and lets
  the agent read what a dialog said.
- New `AutomationRequest` fields: `key`, `dx`, `dy`, `to`, `to_selector`, `accept`,
  `prompt_text`.

**Rust — `crates/opex-core/src/agent/pipeline/tool_defs.rs`:**
- Extend the `browser_action` `action` enum with `scroll`, `hover`, `drag`, `back`,
  `press`, `set_dialog`.
- Add schema properties: `key`, `dx`, `dy`, `to`, `to_selector`, `accept`, `prompt_text`,
  with descriptions. Update the tool description to mention scroll/hover/drag.

### Files
- `docker/browser-renderer/app.py`
- `docker/browser-renderer/test_dispatch.py` (new)
- `crates/opex-core/src/agent/pipeline/tool_defs.rs`

### Tests
- `pytest docker/browser-renderer/test_dispatch.py`: inject a fake `Page` into a session,
  call `dispatch_action` for each new action, assert it invokes the right Playwright method
  with the right args, and that unknown actions raise. Dialog: assert the handler records
  the message and respects accept/dismiss.
- Rust: assert the `browser_action` tool def enum includes the new actions (if the builder
  is reachable from a test; otherwise covered by compile + manual server check).

### Local-verifiable: ⚠️ Python dispatch logic yes (fake page); full Playwright on server.

---

## Component C — Session-scoped `todo` tool

### Problem
Agents have no structured task list that survives context compression. They currently write
free-form `notes/todo.md` (unstructured, agent-scoped, lost after compaction). Multi-step
task reliability suffers.

### Design — storage decision: **DB-backed** (approved)
DB-backed beats in-memory because OPEX resumes sessions across restarts and is
DB-centric; a todo list must survive a restart to be trustworthy.

**Migration `migrations/054_session_todos.sql`:**
```sql
CREATE TABLE session_todos (
    session_id  UUID    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    item_id     TEXT    NOT NULL,
    content     TEXT    NOT NULL,
    status      TEXT    NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending','in_progress','done','cancelled')),
    position    INT     NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (session_id, item_id)
);
```

**`crates/opex-core/src/db/todos.rs` (new):**
- `list_todos(db, session_id) -> Vec<TodoItem>` (ordered by `position`).
- `replace_todos(db, session_id, items)` — full replace in a transaction.
- `merge_todos(db, session_id, items)` — upsert by `item_id`.
- `clear_todos(db, session_id)`.

**Tool `todo`** (`tool_defs.rs` + handler in `agent/pipeline/handlers.rs`):
- Input: `mode: "read" | "write"`, `items: [{id, content, status}]`,
  `strategy: "merge" | "replace"` (default `merge`).
- Limits (mirror Hermes): ≤ 256 items, ≤ 4000 chars per `content`.
- Returns the current formatted list.

**Pure logic split for testability:** the merge/replace reconciliation and the
`format_for_injection(items) -> String` live as pure functions (no DB) so they are
unit-tested without Postgres.

**Context injection** (`crates/opex-core/src/agent/context_builder.rs`): each turn,
load the session's todos and, if non-empty, append an `## Active TODO` block to the built
context. This keeps the list present every turn regardless of compaction.

### Files
- `migrations/054_session_todos.sql` (new)
- `crates/opex-core/src/db/todos.rs` (new) + `db/mod.rs` export
- `crates/opex-core/src/agent/pipeline/tool_defs.rs`
- `crates/opex-core/src/agent/pipeline/handlers.rs`
- `crates/opex-core/src/agent/context_builder.rs`
- `crates/opex-types` if a shared `TodoItem` type is needed

### Tests
- Unit (no DB, local): merge vs replace reconciliation; `format_for_injection`; limit
  enforcement (item count, content length).
- DB (`#[sqlx::test]`, `make test-db`): replace/merge/list/clear round-trips; cascade
  delete with session.

### Local-verifiable: ✅ pure logic; ⚠️ DB round-trips need `make test-db`.

---

## Component D — Prompt-injection: warn → block for identity files

### Problem
`detect_prompt_injection` (`tools/content_security.rs`) is **log-only**: `scan_and_warn`
in `workspace.rs` emits `tracing::warn!` but the content still enters the system prompt
unchanged. `SOUL.md` / `IDENTITY.md` are loaded verbatim into *every* system prompt
(`workspace.rs:221`), so a malicious identity file can hijack the agent. The pattern set is
also narrow (~14 patterns; no C2/exfil/persistence classes).

### Design — blocking scope decision: **targeted** (approved)
Block only the **priority identity files** (`SOUL.md`, `IDENTITY.md`) on a **High-severity**
match — substitute the offending content with a placeholder and emit a warning. All other
workspace files keep the existing **warn-only** behaviour (avoid false positives breaking
legitimate notes/docs).

**`crates/opex-core/src/tools/content_security.rs`:**
- Add `enum Severity { Low, High }`; each entry in `INJECTION_PATTERNS` carries a severity.
- Extend patterns with new classes (High severity):
  - **C2 / Brainworm**: `register (yourself )?as a node`, `beacon`/`heartbeat` to URL,
    `pull (down )?tasking`.
  - **Exfiltration**: piping data out (`curl … | sh`, `wget … | bash`,
    `cat … | curl`), posting secrets to external URLs.
  - **Persistence**: `authorized_keys`, SSH backdoor, modifying agent config / `SOUL.md`
    from within content.
- New `scan_for_block(content) -> BlockVerdict { highest_severity, matched: Vec<String> }`.
- Keep existing `detect_prompt_injection` for the warn path (backward compatible).

**`crates/opex-core/src/agent/workspace.rs`:** when loading a priority identity file
into context, run `scan_for_block`; if `High`, replace the file's contributed content with
a placeholder (e.g. `[CONTENT BLOCKED: potential prompt injection detected]`), `warn!`, and
do not abort the rest of context building. Non-priority files: unchanged `scan_and_warn`.

### Files
- `crates/opex-core/src/tools/content_security.rs`
- `crates/opex-core/src/agent/workspace.rs`

### Tests
- `cargo test` (pure, local): High-severity patterns → block verdict; Low-severity → warn
  only; legitimate identity content → pass; placeholder substitution applied only to
  priority files; new C2/exfil/persistence patterns each detected; benign text containing
  near-miss phrases not falsely blocked.

### Local-verifiable: ✅

---

## Implementation order

1. **A** (email) — warm-up, trivial, isolated.
2. **D** (security) — pure Rust, strongest TDD fit, no deps.
3. **C** (todo) — DB + tool + injection.
4. **B** (browser) — Python + schema, last (browser-renderer rebuild on server).

## Deploy / verification path
- Local: `make check`, `make test` (skips DB tests), `make lint`, `pytest` for browser.
- DB tests: `make test-db`.
- Server: `make remote-deploy` (Rust) + `scp` browser-renderer + container rebuild for B;
  `make doctor` health check; manual smoke of each feature.
