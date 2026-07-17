# Dead-Code & Dead-Route Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Safely remove unambiguously dead code and dead API routes across the OPEX
monorepo (Rust, UI, toolgate, channels, DB schema) without changing runtime behaviour.

**Architecture:** Five deploy-aligned batches executed and deployed in order (Rust+codegen →
UI / toolgate+channels → DB migration → dead routes), plus a final doc-rot sweep. Each task
is a coherent group of deletions verified by a green build/test suite and committed atomically.

**Tech Stack:** Rust 2024 (cargo, sqlx, ts-rs codegen), Next.js 16 / React 19 / vitest (UI),
Python/FastAPI (toolgate), TypeScript/Bun (channels), PostgreSQL 17.

## Cleanup-Specific Method (READ FIRST — replaces TDD)

This plan **deletes** code; you cannot write a failing test for a deletion. Every deletion
task therefore follows this cycle instead of red-green-refactor:

1. **Confirm-dead:** run the exact `grep`/search shown. Expected: only the definition site
   (and possibly `#[cfg(test)]`/doc-comment references) appear. **If a real consumer appears,
   STOP** — the audit is stale for that item; skip it and report, do not delete blindly.
2. **Delete** the code (exact symbols listed).
3. **Fix orphans:** the compiler / type-checker points to any now-dangling reference; remove it.
4. **Verify:** run the build/test command shown. Expected: GREEN.
5. **Commit** with the message shown.

`grep` here means the Grep tool (ripgrep). `rg -w name` matches whole word `name`.

## Global Constraints (apply to every task)

- **Never push or deploy without explicit user approval.** Work directly in `master` (no
  feature branches). Commits are local until approved.
- **No Co-Authored-By / Claude attribution** in commit messages.
- **Rust tests do not run on Windows** — local `cargo check` compiles, but the authoritative
  test/clippy run is on the server: `CARGO_BUILD_JOBS=4 nice ionice -c3 cargo test` detached,
  `make lint` (clippy `-D warnings` — NOT covered by `cargo check`). Deploy = `make remote-deploy`.
- **vitest runs ONLY from `ui/`** (never from repo root — CWD gotcha). UI deploy = `deploy-ui.sh`.
- **rustls-tls only** — never introduce OpenSSL (not relevant to deletions, but do not "fix" by adding deps).
- **Codegen order is load-bearing:** Batch 1 (opex-types + `gen-types`) must be deployed
  BEFORE Batches 2/3 consume regenerated TS types.
- **Reserves to KEEP untouched:** `wipe_agent_memory`, `SinkError::Full`, `usage_log.status` +
  `insert_aborted_row`/`STATUS_ABORTED*`, `set_shutdown_drain_reason`/`CancelReason::ShutdownDrain`,
  deprecated tables `file_scenarios`/`file_scenario_outcomes`/`video_jobs`, `pending_messages`.
- **Between batches:** deploy, then `make doctor` (green) + `make logs` (no new errors) before
  starting the next batch. A broken batch is reverted with `git revert` independently.
- Source of truth for what is dead: `docs/architecture/2026-07-17-dead-code-architecture-audit.md`.
  Design decisions: `docs/superpowers/specs/2026-07-17-dead-code-cleanup-design.md`.

---

## BATCH 1 — Rust (opex-core, opex-db, opex-types, other crates) + codegen

Deploy target after all Batch-1 tasks: `make remote-deploy`. Verify server build is green,
clippy clean, codegen has no drift, then `make doctor`.

### Task 1.1: Dead functions in opex-db

**Files:**
- Modify: `crates/opex-db/src/sessions.rs` (remove `create_isolated_session_with_user`,
  `claim_session_running`, `get_last_user_message`)
- Modify: `crates/opex-db/src/shares.rs` (remove `token_for_session`)

**Interfaces:**
- Produces: nothing (pure removal). Confirms `claim_session_running_with_mode`,
  `get_last_user_message_with_id`, `create_new_session` remain the live variants.

- [ ] **Step 1: Confirm-dead**

Run (Grep tool, whole-word, whole repo):
```
rg -w "create_isolated_session_with_user|claim_session_running|get_last_user_message|token_for_session" --type rust
```
Expected: each name appears ONLY at its definition and in doc-comments/other-variant comments.
`claim_session_running_with_mode` and `get_last_user_message_with_id` are DIFFERENT names — their
hits are fine. If any name appears in a non-test call site, STOP and report.

- [ ] **Step 2: Delete the four functions** (full `pub (async) fn` bodies) from the two files.

- [ ] **Step 3: Verify compile**

Run: `cargo check -p opex-db`
Expected: compiles clean (no "unused" — they were `pub`; no "not found" errors).

- [ ] **Step 4: Commit**
```bash
git add crates/opex-db/src/sessions.rs crates/opex-db/src/shares.rs
git commit -m "chore(db): remove dead session/share helper functions"
```

### Task 1.2: Dead functions in opex-core

**Files:**
- Modify: `crates/opex-core/src/mcp/mod.rs` (remove `load_mcp_prompt`)
- Modify: `crates/opex-core/src/agent/providers/gemini_cloudcode/oauth/flow.rs` (remove `login_code_flow`)
- Modify: `crates/opex-core/src/agent/pipeline/sink.rs` (remove `stream_shapes`)
- Modify: `crates/opex-core/src/agent/providers/cassette_transport.rs` (remove `with_mode`)

- [ ] **Step 1: Confirm-dead**
```
rg -w "load_mcp_prompt|login_code_flow|stream_shapes" --type rust
rg "fn with_mode" crates/opex-core/src/agent/providers/cassette_transport.rs
rg -w with_mode crates/opex-core --type rust
```
Expected: `load_mcp_prompt`, `login_code_flow`, `stream_shapes` — definition + doc-comments only.
`with_mode` — definition only (it is `#[allow(dead_code)]`). STOP if a real caller appears.

- [ ] **Step 2: Delete** the four functions. Also remove the now-unused `#[allow(dead_code)]`
  attribute lines that sat directly above `with_mode` and `stream_shapes`.

- [ ] **Step 3: Verify compile**

Run: `cargo check -p opex-core`
Expected: compiles clean.

- [ ] **Step 4: Commit**
```bash
git add crates/opex-core/src/mcp/mod.rs crates/opex-core/src/agent/providers/gemini_cloudcode/oauth/flow.rs crates/opex-core/src/agent/pipeline/sink.rs crates/opex-core/src/agent/providers/cassette_transport.rs
git commit -m "chore(core): remove dead mcp/oauth/sink/cassette helpers"
```

### Task 1.3: TEST_ONLY `pub` → `#[cfg(test)]` or removed

**Files:**
- Modify: `crates/opex-core/src/agent/session_agent_pool.rs` (`insert_pool_with_cap`)
- Modify: `crates/opex-core/src/agent/agent_state.rs` (`active_request_count`, `unregister_request`)
- Modify: `crates/opex-core/src/agent/hooks.rs` (`first_matcher_matches`)
- Modify: `crates/opex-core/src/agent/providers/gemini_cloudcode/oauth/device.rs` (`poll_device_flow`)
- Modify: `crates/opex-core/src/agent/commands/spec.rs` (`sanitize_native_name`)
- Modify: `crates/opex-gateway-util/src/config_path.rs` (`resolve_config_path_in`)
- Modify: `crates/opex-core/src/agent/engine_event_sender.rs` (`send`, `inner`)

Rule: if the ONLY callers are inside a `#[cfg(test)] mod tests` in the same file, delete the
`pub` symbol AND the tests that exercised only it (the tests validate nothing reachable in prod).
Exception noted below for `insert_pool_with_cap`.

- [ ] **Step 1: Confirm test-only** — for each symbol:
```
rg -w insert_pool_with_cap --type rust
rg -w "active_request_count|unregister_request|first_matcher_matches|poll_device_flow|sanitize_native_name|resolve_config_path_in" --type rust
rg -n "EngineEventSender::send|\.inner\(\)" crates/opex-core --type rust
```
Expected: every non-definition hit is inside a `#[cfg(test)]` block. Verify by opening each
hit's enclosing module. STOP if any prod caller exists.

- [ ] **Step 2: Delete** each symbol and its test-only callers. For `poll_device_flow`,
  confirm prod uses `device_flow::poll_once` (leave that). For `EngineEventSender`, confirm prod
  uses `send_async` (leave that).

- [ ] **Step 3: Special case — `insert_pool_with_cap`.** Its LRU-eviction logic is duplicated
  (inlined) in `agent_tool::ask_spawn_new`. Deleting the tested copy leaves the inlined prod copy
  untested. Add a one-line comment at the inlined site in `agent_tool` (grep `ask_spawn_new` to
  find it):
```rust
// NOTE: pool eviction is inlined here; dedup to a shared helper tracked in the "A" fix cycle.
```

- [ ] **Step 4: Verify compile**

Run: `cargo check -p opex-core -p opex-gateway-util --all-targets`
Expected: compiles clean including test targets.

- [ ] **Step 5: Commit**
```bash
git add -A
git commit -m "chore(core): drop test-only pub symbols and their vacuous tests"
```

### Task 1.4: Dead enum variants (opex-core + opex-catalog)

**Files:**
- Modify: `crates/opex-core/src/agent/stream_event.rs` (`StreamEvent::AgentSwitch`)
- Modify: `crates/opex-core/src/agent/engine/stream.rs` (`ProcessingPhase::CallingTool`, `::Composing`)
- Modify: `crates/opex-core/src/agent/pipeline/sink.rs` (`SinkError::Fatal` — **keep `Full`**)
- Modify: `crates/opex-core/src/agent/hooks.rs` (`HookEvent::AfterResponse`, `::OnError`)
- Modify: `crates/opex-core/src/agent/providers/error.rs` (`PartialState::Thinking`)
- Modify: `crates/opex-catalog/src/lib.rs` (`CatalogSource::OpenRouter`, `::LiteLlm`;
  `Caps::{attachment, reasoning, tool_call}`)

- [ ] **Step 1: Confirm-dead** — for each variant, confirm it is never CONSTRUCTED (only matched):
```
rg -w "AgentSwitch|Composing|Thinking|OpenRouter|LiteLlm" --type rust
rg "SinkError::Fatal|HookEvent::AfterResponse|HookEvent::OnError|ProcessingPhase::CallingTool" --type rust
```
Expected: no construction site (`Variant { .. }` / `Variant(..)` on the build side) outside tests.
`SinkError::Full` MUST remain — do not touch it. STOP if a real emitter exists.

- [ ] **Step 2: Delete** each variant. The compiler will flag now-unreachable `match` arms —
  remove those arms too. For `Caps::{attachment,reasoning,tool_call}`, remove the fields and any
  struct-literal initialisers of them in `opex-catalog`.

- [ ] **Step 3: Verify compile**

Run: `cargo check -p opex-core -p opex-catalog --all-targets`
Expected: compiles clean (no non-exhaustive-match errors).

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(core): remove never-emitted enum variants (keep SinkError::Full reserve)"
```

### Task 1.5: Dead protocol variants in opex-types + regenerate TS

**Files:**
- Modify: `crates/opex-types/src/channels.rs` (`ChannelOutbound::Reload`)
- Modify: `crates/opex-types/src/ws.rs` (`WsEvent::AuditEvent`)
- Regenerate: `ui/src/stores/ws.generated.ts` and channels `types.generated.ts` (via `gen-types`)

**Interfaces:**
- Produces: regenerated TS type files WITHOUT `Reload`/`AuditEvent`. Batches 2 (UI) and 3
  (channels) consume these — they remove the corresponding handlers.

- [ ] **Step 1: Confirm-dead (Rust side)**
```
rg "ChannelOutbound::Reload|WsEvent::AuditEvent" --type rust
```
Expected: construction only in tests (or nowhere). STOP if a prod send-site builds them.

- [ ] **Step 2: Delete** both variants and any test fixtures/match arms that reference them.

- [ ] **Step 3: Regenerate TS types.** Find the codegen command (grep `gen-types` in `Makefile`):
```bash
make gen-types
```
Expected: `ui/src/stores/ws.generated.ts` and `channels/src/types.generated.ts` change to drop
the removed variants; `git status` shows exactly those generated files modified.

- [ ] **Step 4: Verify compile + no drift**

Run: `cargo check -p opex-types`
Then re-run `make gen-types` a SECOND time and `git diff --exit-code` on the generated files.
Expected: compiles clean; second run produces no further diff (codegen is stable).

- [ ] **Step 5: Commit**
```bash
git add crates/opex-types/src/channels.rs crates/opex-types/src/ws.rs ui/src/stores/ws.generated.ts channels/src/types.generated.ts
git commit -m "chore(types): remove dead Reload/AuditEvent protocol variants + regen"
```

### Task 1.6: Dead config keys (except AgentSettings — Task 1.7)

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (`VideoConfig` 6 keys + their default fns;
  `ToolConfig.protocol`, `.api_key_env`; `McpConfig.protocol`, `McpFileEntry.protocol`)
- Modify: `crates/opex-core/src/db/providers.rs` (`CAPABILITY_COMPACTION`)

- [ ] **Step 1: Confirm-dead**
```
rg -w "scene_threshold|frame_ceiling|job_timeout_secs|note_max_frames|vault_name" crates/opex-core --type rust
rg "CAPABILITY_COMPACTION" --type rust
rg -n "\.protocol|api_key_env" crates/opex-core/src/config/mod.rs
```
Expected: `VideoConfig`'s 6 fields read nowhere (keep `digest_provider`/`digest_model` —
they ARE read). `url_allowlist` is the 6th field name; confirm it too. `ToolConfig.protocol`,
`.api_key_env`, `McpConfig.protocol` constructed as `None`/default and never read.
`CAPABILITY_COMPACTION` — legacy const, resolution now via profile slot. STOP on any real reader.

- [ ] **Step 2: Delete** the 6 `VideoConfig` fields + their `#[serde(default = "...")]` default
  functions; `ToolConfig.protocol`/`.api_key_env`; `McpConfig.protocol` + `McpFileEntry.protocol`;
  `CAPABILITY_COMPACTION` const. Remove any struct-literal initialisers the compiler flags.

- [ ] **Step 3: Verify compile**

Run: `cargo check -p opex-core --all-targets`
Expected: compiles clean.

- [ ] **Step 4: Commit**
```bash
git add crates/opex-core/src/config/mod.rs crates/opex-core/src/db/providers.rs
git commit -m "chore(config): remove unread config keys (VideoConfig/ToolConfig/McpConfig/COMPACTION)"
```

### Task 1.7: Remove AgentSettings migration keys + profile migration

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (`AgentSettings.{provider, model,
  provider_connection, fallback_provider, tts_provider, imagegen_provider}`)
- Delete: `crates/opex-core/src/db/profile_migration.rs`
- Modify: `crates/opex-core/src/main.rs` (remove the startup call to the profile migration)

- [ ] **Step 1: Map the call site**
```
rg -n "profile_migration" crates/opex-core --type rust
```
Expected: the module declaration (`mod profile_migration;`), the startup invocation in `main.rs`,
and reads of the six `AgentSettings` fields INSIDE `profile_migration.rs` only. If those fields
are read anywhere else, STOP and report (they would not be migration-only).

- [ ] **Step 2: Delete** `profile_migration.rs`, its `mod profile_migration;` declaration, the
  `main.rs` startup call, and the six `AgentSettings` fields (+ their serde defaults).

- [ ] **Step 3: Verify compile**

Run: `cargo check -p opex-core`
Expected: compiles clean.

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(config): drop one-time profile migration and its AgentSettings keys"
```

### Task 1.8: Remove orphan crate + memory-worker otel feature

**Files:**
- Delete: `crates/opex-migrate-checksums/` (whole directory)
- Modify: `Cargo.toml` (workspace root — remove member `crates/opex-migrate-checksums`)
- Delete: `crates/opex-memory-worker/src/otel_init.rs`
- Modify: `crates/opex-memory-worker/Cargo.toml` (remove `otel` feature + its optional deps if
  not shared)
- Modify: `crates/opex-memory-worker/src/main.rs` (remove otel feature-gated branches + `mod otel_init;`)

- [ ] **Step 1: Confirm orphan + unused feature**
```
rg "opex-migrate-checksums" --type-not rust
rg -n "otel" crates/opex-memory-worker
rg "features.*otel|--features otel" Makefile scripts release.sh .github 2>/dev/null
```
Expected: `opex-migrate-checksums` referenced only in root `Cargo.toml` members + docs (not in
Makefile/scripts/CI). memory-worker `otel` not enabled by any build path. `opex-core` otel is
separate — do NOT touch it. STOP if a build path enables memory-worker otel.

- [ ] **Step 2: Delete** the crate directory, its workspace member line, `otel_init.rs`, the
  `mod otel_init;` line, the `#[cfg(feature = "otel")]` branches in memory-worker `main.rs`, and
  the `otel` feature stanza in its `Cargo.toml`.

- [ ] **Step 3: Verify workspace resolves + compiles**

Run: `cargo check --workspace`
Expected: workspace resolves without the removed member; all crates compile.

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore: remove orphan opex-migrate-checksums crate and unused memory-worker otel"
```

### Task 1.9: De-lint stale `#[allow(dead_code)]` on LIVE code

**Files (remove ONLY the attribute, code stays):**
- `crates/opex-core/src/agent/dispatcher/lookup.rs` (`is_valid_tool_name`, `find_extension_tool`)
- `crates/opex-core/src/agent/lsp/*` (client methods, manager, servers — "Task 6/7 landed")
- `crates/opex-core/src/db/agent_plans.rs` (`DayIntent`, `day_plan_*`)
- `crates/opex-core/src/gateway/clusters/auth_services.rs`
- `crates/opex-core/src/db/uploads.rs` (`mint_codemode_token`)

- [ ] **Step 1: Prove each symbol is LIVE** (has a real caller):
```
rg -w "is_valid_tool_name|find_extension_tool|mint_codemode_token" --type rust
rg -n "day_plan" crates/opex-core/src/initiative --type rust
```
Expected: each has a non-test prod caller (e.g. `mint_codemode_token` in
`tool_handlers/orchestrate.rs`, `day_plan_*` in `initiative/day_plan.rs`). Only then is the
`#[allow(dead_code)]` a stale lie safe to remove. If truly unused, LEAVE it (it is not this task's job).

- [ ] **Step 2: Remove** just the `#[allow(dead_code)]` / `#[allow(unused)]` attribute lines on
  those live symbols. Do NOT remove FromRow/diagnostic-field allows (sqlx structs) — those are
  legitimate.

- [ ] **Step 3: Verify no new warnings**

Run (server, authoritative): `make lint`
Expected: clippy passes with `-D warnings` — i.e. removing the allow did NOT surface a real
dead-code warning (which would mean the symbol was actually dead; if so, restore the allow and
report). Locally `cargo check` will not show dead_code for `pub`; rely on the server lint.

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(core): drop stale allow(dead_code) markers on live code"
```

### Task 1.10: Batch-1 doc-rot + server verification gate

**Files:**
- Modify: `crates/opex-core/src/agent/service_registry.rs` (fix the doc-comment claiming files
  live in `workspace/tools/*.yaml` → actually `config/services/`)
- Modify: `crates/opex-core/src/agent/handler_registry.rs` (fix comment referencing nonexistent
  `/api/handlers/enqueue` → real path `POST /api/files/run|menu-run`)
- Modify: `crates/opex-core/src/agent/memory/store.rs` (update `wipe_agent_memory` comment to
  "reserved for the agent-deletion completeness plan", so it is not mistaken for dead code)

- [ ] **Step 1** Apply the three comment edits above (text only, no code change).

- [ ] **Step 2: Full server verification** (authoritative gate before deploy):
```bash
# on server ~/opex-src after the branch is pushed, OR via make remote-build
make remote-build            # cargo build --release, no swap
# then, detached, the test + lint suite:
CARGO_BUILD_JOBS=4 nice ionice -c3 cargo test --workspace
make lint
```
Expected: release build green, full test suite green, clippy `-D warnings` green. Re-run
`make gen-types` and `git diff --exit-code` on generated files — no drift.

- [ ] **Step 3: Commit**
```bash
git add -A
git commit -m "docs(core): fix service-registry/handler-registry/wipe-memory comments"
```

- [ ] **Step 4: DEPLOY (requires user approval to push).** After approval:
```bash
make remote-deploy
make doctor      # expect green
make logs        # expect no new errors
```
This deploy carries Tasks 1.1–1.10. **Do not start Batch 2/3 until this deploy is green** —
they depend on the regenerated TS types shipped here.

---

## BATCH 2 — UI

Deploy target after all Batch-2 tasks: `deploy-ui.sh`. All verification from `ui/`.

### Task 2.1: Delete unimported UI component files

**Files:**
- Delete: `ui/src/components/ui/scroll-area.tsx`
- Delete: `ui/src/components/ui/progress.tsx`
- Delete: `ui/src/components/workspace/markdown-editor.tsx`
- Modify: `ui/src/__tests__/pages-smoke.test.tsx` (remove the `MarkdownEditor` import + its test case)

- [ ] **Step 1: Confirm-dead**
```
rg "scroll-area|ScrollArea" ui/src
rg -w "Progress" ui/src/components ui/src/app
rg "markdown-editor|MarkdownEditor" ui/src
```
Expected: `scroll-area`/`progress` — zero imports anywhere. `MarkdownEditor` — imported ONLY in
`pages-smoke.test.tsx`; prod uses `obsidian-editor.tsx` (confirm it exists and is imported). STOP
if any prod import appears.

- [ ] **Step 2: Delete** the three files; remove the `MarkdownEditor` import and its test block
  from `pages-smoke.test.tsx`.

- [ ] **Step 3: Verify**
```bash
cd ui && npx tsc --noEmit && npm test
```
Expected: tsc clean; vitest green (the removed test is gone, others pass).

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(ui): delete unimported scroll-area/progress/markdown-editor"
```

### Task 2.2: Remove dead React Query hooks + their vacuous mocks

**Files:**
- Modify: `ui/src/lib/queries.ts` (remove `useTools`, `useProviderModels`, `useCuratorConfig`,
  `useUpdateAgent`, `useRestartService`, `useRebuildService`, `useHandlerAllowlist`,
  `useCreateHandler`, `useUpdateHandler`)
- Modify: ~20 test files that mock these (compiler/grep will list them)

- [ ] **Step 1: Confirm-dead (prod) + list mocks**

For each hook, prove no non-test prod caller:
```
rg -w "useTools|useProviderModels|useCuratorConfig|useUpdateAgent|useRestartService|useRebuildService|useHandlerAllowlist|useCreateHandler|useUpdateHandler" ui/src
```
Expected: every hit is either the definition in `queries.ts`, a `vi.mock`/mock-return in a
`*.test.tsx`, or absent from prod components. Confirm prod uses `useProviderModelsDetailed` (leave
it). STOP if a prod component imports any of these.

- [ ] **Step 2: Delete** the nine hook definitions. Then remove each hook's mock lines from every
  test file the grep listed (a mock of a now-deleted hook references nothing and is noise).

- [ ] **Step 3: Verify**
```bash
cd ui && npx tsc --noEmit && npm test
```
Expected: tsc clean; vitest green.

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(ui): remove dead query hooks and their vacuous test mocks"
```

### Task 2.3: Remove dead exports (stores, api, types)

**Files:**
- Modify: `ui/src/stores/chat-selectors.ts` (`selectActiveSessionId`, `useSelectedBranches`,
  `selectSelectedBranches`)
- Modify: `ui/src/stores/chat-persistence.ts` (`clearLastSessionId`)
- Modify: `ui/src/stores/chat-types.ts` (`MESSAGES_HISTORY_LIMIT`)
- Modify: `ui/src/lib/api.ts` (`apiGetBlob`, `unshareSession`, `inviteAgent`)
- Modify: `ui/src/types/api.ts` (`FileActionButton`, `FileActionsResponse`, `AgentToolConfig`)
- Modify: `ui/src/types/ws.ts` (remove all back-compat aliases EXCEPT `WsLog`)

- [ ] **Step 1: Confirm-dead**
```
rg -w "selectActiveSessionId|useSelectedBranches|selectSelectedBranches|clearLastSessionId|MESSAGES_HISTORY_LIMIT|apiGetBlob|unshareSession|inviteAgent|FileActionButton|FileActionsResponse|AgentToolConfig" ui/src
rg "WsSessionUpdated|WsFile|WsSessionDeleted" ui/src   # sample the ws.ts aliases
```
Expected: each symbol appears at its definition only (or definition + one test for `inviteAgent`).
For `ws.ts` aliases: only `WsLog` has real consumers; the other 16 aliases have none. STOP if a
real consumer appears for anything else.

- [ ] **Step 2: Delete** the listed exports; from `ws.ts` keep only `WsLog`.

- [ ] **Step 3: Verify**
```bash
cd ui && npx tsc --noEmit && npm test
```
Expected: tsc clean (this is the real gate for unused exports); vitest green.

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(ui): remove dead store/api/type exports"
```

### Task 2.4: Remove dead i18n keys + default SVGs

**Files:**
- Modify: `ui/src/i18n/locales/en.json`, `ui/src/i18n/locales/ru.json` (~32 dead keys each)
- Delete: `ui/public/file.svg`, `globe.svg`, `next.svg`, `vercel.svg`, `window.svg`

- [ ] **Step 1: Confirm-dead (i18n).** For each candidate key, prove no `t("key")` usage. Start
  with the confirmed cluster and expand:
```
rg "context_window_" ui/src        # confirmed-removed feature cluster
rg "error_api|agent_joined|section_watchdog" ui/src
```
For a full pass, list every leaf key and grep it; a key used only via a template string (`t(\`x.${v}\`)`)
must be treated as LIVE — do NOT remove keys reachable by any dynamic prefix. Plural suffixes
(`_one/_few/_many/_other`) resolve dynamically — keep them.

- [ ] **Step 2: Confirm-dead (SVG)**
```
rg "file.svg|globe.svg|next.svg|vercel.svg|window.svg" ui/src
```
Expected: zero references. STOP otherwise.

- [ ] **Step 3: Delete** the confirmed-dead keys from BOTH locale files (keep en/ru in sync) and
  the five SVGs.

- [ ] **Step 4: Verify**
```bash
cd ui && npx tsc --noEmit && npm test && npm run build
```
Expected: all green (build ensures no missing-asset/import references).

- [ ] **Step 5: Commit + DEPLOY (requires approval)**
```bash
git add -A
git commit -m "chore(ui): prune dead i18n keys and default SVGs"
# after approval:
bash deploy-ui.sh
```
Then verify the UI loads and chat renders (smoke check). Batch 2 complete.

---

## BATCH 3 — toolgate + channels

Deploy: toolgate = scp changed `.py` + `POST /api/services/toolgate/restart`; channels = restart.
Can run in parallel with Batch 2 (both depend only on Batch 1's deploy).

### Task 3.1: Remove dead toolgate endpoints

**Files:**
- Modify: `toolgate/routers/video.py` (remove `POST /summarize-video`)
- Modify: `toolgate/routers/tts.py` (remove `POST /tts`)
- Modify: `toolgate/handlers/router.py` (remove `GET /handlers/{handler_id}`)
- Modify: corresponding tests under `toolgate/tests/` that hit these paths

- [ ] **Step 1: Confirm-dead (cross-repo, since consumers are in Rust/YAML/UI)**
```
rg "summarize-video|/summarize_video" crates ui channels workspace config
rg "9011/tts|\"/tts\"|'/tts'" crates ui channels workspace config
rg "handlers/\{.*\}\"|handlers/%s" crates   # the GET-one-handler path
```
Expected: zero consumers outside toolgate. `summarize_video` work runs in-process via
`video_helpers`; TTS consumers all use `/v1/audio/speech`. STOP if a consumer appears.

- [ ] **Step 2: Delete** the three route handlers + their tests.

- [ ] **Step 3: Verify**
```bash
cd toolgate && python -m pytest -q
```
Expected: green (fewer tests; no import errors).

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(toolgate): remove dead /summarize-video, /tts, GET /handlers/{id}"
```

### Task 3.2: Remove dead toolgate code + fix network-hitting tests

**Files:**
- Modify: `toolgate/registry.py` (remove `UTILITY_SERVICES`, dead `_aload_config_from_api` import)
- Modify: `toolgate/config.py` (remove `aload_config`)
- Modify: `toolgate/workspace_helpers.py` (remove `get_secret` — SEE server check)
- Modify: `toolgate/tests/test_registry.py`, `test_config.py` (remove vacuous monkeypatch tests)

- [ ] **Step 1: Confirm-dead + server check for get_secret**
```
rg "UTILITY_SERVICES|_aload_config_from_api|aload_config|get_secret" toolgate
```
Expected: `UTILITY_SERVICES` — self only; `aload_config` — test only; `_aload_config_from_api`
import — unused in `registry.py` body. For `get_secret`: it is a public API intended for external
`workspace/file_handlers/*.py`. **Before deleting, grep the SERVER** for out-of-git handlers:
`ssh <server> "rg -w get_secret ~/opex/workspace"`. If any server handler uses it, KEEP `get_secret`
and skip only the others.

- [ ] **Step 2: Delete** confirmed-dead symbols. Remove the `test_registry.py` monkeypatch tests
  that patched `_aload_config_from_api` (they validated nothing and hit the network) and the
  `test_config.py::aload_config` test.

- [ ] **Step 3: Verify (offline)**
```bash
cd toolgate && python -m pytest -q
```
Expected: green AND fast (no multi-second network-timeout stalls — proving the network-hitting
tests are gone).

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "chore(toolgate): remove dead registry/config helpers + network-hitting tests"
```

### Task 3.3: Remove dead channels code, deps, and Reload handling

**Files:**
- Modify: `channels/src/drivers/common.ts` (remove `decodeBase64Param`)
- Modify: `channels/src/formatting.ts` (remove `loadedChannels`)
- Modify: `channels/src/session.ts`, `channels/src/bridge.ts` (remove `Reload` handling)
- Modify: `channels/package.json` (remove `irc-framework`, `matrix-bot-sdk`, `@opentelemetry/api`)
- Modify: relevant `channels/src/__tests__/*` (remove tests of deleted symbols/fixtures)

- [ ] **Step 1: Confirm-dead**
```
rg -w "decodeBase64Param|loadedChannels" channels/src
rg -w "reload|Reload" channels/src
rg "irc-framework|matrix-bot-sdk|@opentelemetry/api" channels/src
```
Expected: `decodeBase64Param` — test only; `loadedChannels` — zero refs; `Reload` — handled in
`session.ts`/`bridge.ts` + a test fixture but never received (Batch 1 removed the emitter). The
three deps — never imported in `src` (irc uses raw `node:net`, matrix uses `fetch`). For
`@opentelemetry/api`, double-check it is not a peer-dep required by the otel sdk packages that
`otel.ts` dynamically imports; if removing it breaks `bun install`, keep it and note so.

- [ ] **Step 2: Delete** the two functions, the `Reload` branches + fixture, and the three deps
  from `package.json`. Run `bun install` to update the lockfile.

- [ ] **Step 3: Verify**
```bash
cd channels && bun install && bun test
```
Expected: install clean; tests green.

- [ ] **Step 4: Commit + DEPLOY (requires approval)**
```bash
git add -A
git commit -m "chore(channels): remove dead helpers, unused deps, Reload handling"
# after approval: scp changed toolgate .py to server + restart toolgate; restart channels
```
Verify via `make doctor` that toolgate + channels are healthy. Batch 3 complete.

---

## BATCH 4 — DB migration (drop dead indexes + edited_at)

Prerequisite: Batch 1 deployed (its Rust code no longer reads `messages.edited_at`).

### Task 4.1: Confirm no code reads edited_at, then write the migration

**Files:**
- Modify (if needed): `crates/opex-db/src/sessions.rs` (remove `edited_at` from SELECT lists +
  DTO mapping — only if Batch 1 did not already)
- Create: `migrations/m088_drop_dead_indexes.sql` (use the next free number after the highest
  existing `migrations/m0NN_*.sql`)

- [ ] **Step 1: Confirm edited_at is no longer read**
```
rg "edited_at" crates
```
Expected: zero hits in code (only in old migration files). If `sessions.rs` still selects/maps it,
remove those first, `cargo check -p opex-db`, and commit `chore(db): stop reading messages.edited_at`.
**Do not proceed to DROP COLUMN while any query still reads it.**

- [ ] **Step 2: Confirm indexes are dead** (no query relies on them — safe because dropping an
  index only affects performance, never correctness; these were shown unused):
```
rg "idx_messages_role|idx_messages_tool_call|idx_stream_running|idx_sessions_agent|idx_sessions_user|idx_session_shares_token|idx_pairing_codes_agent" migrations crates
```
Expected: hits only in the CREATE statements in old migrations.

- [ ] **Step 3: Write the migration** — `migrations/m088_drop_dead_indexes.sql`:
```sql
-- Drop indexes shown dead/duplicate by the 2026-07-17 audit.
DROP INDEX IF EXISTS idx_messages_role;
DROP INDEX IF EXISTS idx_messages_tool_call;
DROP INDEX IF EXISTS idx_stream_running;
DROP INDEX IF EXISTS idx_sessions_agent;      -- superseded by m022/m072 composites
DROP INDEX IF EXISTS idx_sessions_user;       -- superseded by m022/m072 composites
DROP INDEX IF EXISTS idx_session_shares_token; -- duplicates UNIQUE(token)
DROP INDEX IF EXISTS idx_pairing_codes_agent; -- duplicates PK prefix (agent_id, code)

-- Drop dead column: written nowhere, always NULL.
ALTER TABLE messages DROP COLUMN IF EXISTS edited_at;
```
Do NOT drop `usage_log.status`/`idx_usage_log_status_aborted` (reserved), deprecated tables, or
`pending_messages`.

- [ ] **Step 4: Verify migration compiles into the build** (sqlx checks migrations at build):

Run: `cargo check -p opex-core`
Expected: clean (migration file is picked up; no macro errors).

- [ ] **Step 5: Commit**
```bash
git add migrations/m088_drop_dead_indexes.sql crates/opex-db/src/sessions.rs
git commit -m "chore(db): drop dead indexes and unused messages.edited_at column"
```

- [ ] **Step 6: DEPLOY (requires approval).** Migration auto-runs on startup:
```bash
make remote-deploy
make doctor        # expect green — confirms migration applied cleanly
make logs          # confirm "applied migration m088" and no errors
```
Batch 4 complete.

---

## BATCH 5 — Dead routes + docs/API.md

Prerequisite: run the server-side external-consumer grep BEFORE deploying.

### Task 5.1: Pre-flight — grep server for external consumers of dead routes

- [ ] **Step 1: List the routes to delete** from the audit (Batch B). Compile the path list into
  a scratch file.

- [ ] **Step 2: Grep the server** for any operator/ops usage the repo cannot show:
```bash
ssh <server> "rg -n 'api/(agents/.*/skills|agents/.*/yaml-tools|memory$|config/export|config/import|auth/google|oauth/providers)' ~/opex /etc/nginx 2>/dev/null; systemctl --user cat opex-core 2>/dev/null | rg -i curl"
```
Expected: no hits. **Any hit → move that route to a KEEP list and report it; do not delete blindly.**

- [ ] **Step 3:** Record the confirmed-deletable list. No commit (investigation only).

### Task 5.2: Delete dead routes + orphaned handlers

**Files (modify — remove `.route(...)` lines and the handler fns they point to):**
- `crates/opex-core/src/gateway/handlers/agents/*` (hooks, context-breakdown, icon DELETE,
  channels/{id}/status), `.../skills.rs`, `.../yaml_tools.rs`, `.../memory.rs` (legacy list/create/
  export/fts-language/{id} DELETE+PATCH/tasks), `.../config.rs` (export/import), `.../curator.rs`
  (preview, runs/{id}), `.../monitoring/*` (usage/sessions, audit/tools, sessions/{id}/failures,
  watchdog/config), `.../cron.rs` (runs), `.../providers.rs` (resolve, PATCH cli_options),
  `.../services.rs` (GET list), `.../skills.rs` (versions/{vid}, snapshot), `.../initiative.rs`
  (plan/day approve+dismiss), `.../files.rs` (actions, run), `.../infra.rs` (decisions POST+GET),
  `.../oauth.rs` (providers), `.../google_auth.rs` (5 device-flow routes),
  `.../agents/*` approvals allowlist GET+DELETE
- Also delete the now-dead route smoke test `crates/opex-core/tests/integration_google_auth_routes.rs`

**KEEP (live external contracts — do NOT delete):** `/v1/*`, `/api/files/jobs/*`,
`/api/uploads/*`, `/api/internal/*`, `/api/sandbox/*`, webhooks, OAuth callback,
`/api/csp-report`, `/api/memory/reindex`, `/api/health/dashboard`.

- [ ] **Step 1** For each route in the confirmed list from Task 5.1, remove its `.route()`
  registration line from the module's `routes()` fn, then delete the handler fn it named. The
  compiler lists any helper left unused — remove those too (unless shared with a KEEP route).

- [ ] **Step 2: Verify compile (server-authoritative for full build)**
```bash
cargo check -p opex-core --all-targets   # local fast check
```
Then on server: `make remote-build` + `CARGO_BUILD_JOBS=4 nice ionice -c3 cargo test --workspace` + `make lint`.
Expected: all green; the google-auth smoke test is gone, nothing else references deleted handlers.

- [ ] **Step 3: Commit**
```bash
git add -A
git commit -m "chore(gateway): remove 48 dead routes and their orphaned handlers"
```

### Task 5.3: Sync docs/API.md + deploy

**Files:**
- Modify: `docs/API.md` (remove the deleted routes' entries)

- [ ] **Step 1** Remove from `docs/API.md` every entry corresponding to a route deleted in 5.2.
  Grep to confirm none of the deleted paths remain documented:
```
rg "agents/.*/skills|config/export|auth/google|oauth/providers" docs/API.md
```
Expected: no stale entries after edit.

- [ ] **Step 2: Commit + DEPLOY (requires approval)**
```bash
git add docs/API.md
git commit -m "docs: sync API.md with removed dead routes"
# after approval:
make remote-deploy
make doctor       # green
make logs         # no errors; UI smoke-check still works
```
Batch 5 complete.

---

## FINAL — Cross-cutting doc-rot sweep

### Task 6.1: Fix top-level docs

**Files:**
- Modify: `CLAUDE.md` (§Graceful Shutdown "Graph worker" line — graph dropped in m018; remove/fix.
  `searxng_search.yaml` example — file does not exist; remove. `make test` note — it actually runs
  `--features gemini-cloudcode`; correct the description.)
- Modify: `docs/ARCHITECTURE.md:505` (replace stale tool names `process_start`, `memory_get`,
  `memory_delete` with real `process`, `memory` action forms)

- [ ] **Step 1: Confirm the rot**
```
rg -n "Graph worker|searxng_search.yaml" CLAUDE.md
rg -n "process_start|memory_get|memory_delete" docs/ARCHITECTURE.md
```
Expected: the stale lines are present.

- [ ] **Step 2** Apply the corrections (documentation text only).

- [ ] **Step 3: Commit**
```bash
git add CLAUDE.md docs/ARCHITECTURE.md
git commit -m "docs: remove stale graph-worker/searxng/process_start references"
```

- [ ] **Step 4: Update the audit + memory.** Mark section B/C items resolved in
  `docs/architecture/2026-07-17-dead-code-architecture-audit.md` (add a "RESOLVED 2026-…" note),
  and update the `MEMORY.md` pointer for
  `project_dead_code_architecture_audit.md` to reflect that cleanup (C→B) shipped and only the
  "A" fix-wave remains. Commit `docs: mark dead-code cleanup batches shipped`.

---

## Self-Review Notes (author checklist — done)

- **Spec coverage:** every spec item mapped — Batch1↔C-Rust (Tasks 1.1–1.10), Batch2↔C-UI
  (2.1–2.4), Batch3↔C-toolgate/channels (3.1–3.3), Batch4↔DB indexes+edited_at (4.1), Batch5↔B
  routes (5.1–5.3), doc-rot sweep (6.1). Kept-reserve decisions (usage_log.status, SinkError::Full,
  wipe_agent_memory, deprecated tables, pending_messages) are explicitly excluded in each relevant task.
- **Decisions honored:** AgentSettings+profile_migration removed (1.7); otel worker removed (1.8);
  UX-gap routes (context-breakdown, allowlist GET/DELETE, unshareSession export) removed (2.3/5.2).
- **Codegen ordering:** Task 1.5 regenerates + 1.10 deploys before Batches 2/3.
- **No placeholders:** each deletion task carries exact paths, exact grep confirm-dead commands,
  and exact verify commands. No "TBD".
